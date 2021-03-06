use std::collections::HashMap;
use std::fmt;
use std::io::fs::{mod, PathExtensions};
use std::os;
use std::slice;
use std::str;
use std::default::Default;
use toml;
use semver;
use serialize::{Decodable, Decoder};

use core::SourceId;
use core::{Summary, Manifest, Target, Dependency, PackageId};
use core::dependency::{Build, Development};
use core::manifest::{LibKind, Lib, Dylib, Profile, ManifestMetadata};
use core::package_id::Metadata;
use util::{CargoResult, Require, human, ToUrl, ToSemver};

/// Representation of the projects file layout.
///
/// This structure is used to hold references to all project files that are relevant to cargo.

#[deriving(Clone)]
pub struct Layout {
    root: Path,
    lib: Option<Path>,
    bins: Vec<Path>,
    examples: Vec<Path>,
    tests: Vec<Path>,
    benches: Vec<Path>,
}

impl Layout {
    fn main(&self) -> Option<&Path> {
        self.bins.iter().find(|p| {
            match p.filename_str() {
                Some(s) => s == "main.rs",
                None => false
            }
        })
    }
}

fn try_add_file(files: &mut Vec<Path>, root: &Path, dir: &str) {
    let p = root.join(dir);
    if p.exists() {
        files.push(p);
    }
}
fn try_add_files(files: &mut Vec<Path>, root: &Path, dir: &str) {
    match fs::readdir(&root.join(dir)) {
        Ok(new) => {
            files.extend(new.into_iter().filter(|f| f.extension_str() == Some("rs")))
        }
        Err(_) => {/* just don't add anything if the directory doesn't exist, etc. */}
    }
}

/// Returns a new `Layout` for a given root path.
/// The `root_path` represents the directory that contains the `Cargo.toml` file.

pub fn project_layout(root_path: &Path) -> Layout {
    let mut lib = None;
    let mut bins = vec!();
    let mut examples = vec!();
    let mut tests = vec!();
    let mut benches = vec!();

    if root_path.join("src/lib.rs").exists() {
        lib = Some(root_path.join("src/lib.rs"));
    }

    try_add_file(&mut bins, root_path, "src/main.rs");
    try_add_files(&mut bins, root_path, "src/bin");

    try_add_files(&mut examples, root_path, "examples");

    try_add_files(&mut tests, root_path, "tests");
    try_add_files(&mut benches, root_path, "benches");

    Layout {
        root: root_path.clone(),
        lib: lib,
        bins: bins,
        examples: examples,
        tests: tests,
        benches: benches,
    }
}

pub fn to_manifest(contents: &[u8],
                   source_id: &SourceId,
                   layout: Layout)
                   -> CargoResult<(Manifest, Vec<Path>)> {
    let manifest = layout.root.join("Cargo.toml");
    let manifest = match manifest.path_relative_from(&os::getcwd()) {
        Some(path) => path,
        None => manifest,
    };
    let contents = try!(str::from_utf8(contents).require(|| {
        human(format!("{} is not valid UTF-8", manifest.display()))
    }));
    let root = try!(parse(contents, &manifest));
    let mut d = toml::Decoder::new(toml::Table(root));
    let toml_manifest: TomlManifest = match Decodable::decode(&mut d) {
        Ok(t) => t,
        Err(e) => return Err(human(format!("{} is not a valid \
                                            manifest\n\n{}",
                                           manifest.display(), e)))
    };

    let pair = try!(toml_manifest.to_manifest(source_id, &layout).map_err(|err| {
        human(format!("{} is not a valid manifest\n\n{}",
                      manifest.display(), err))
    }));
    let (mut manifest, paths) = pair;
    match d.toml {
        Some(ref toml) => add_unused_keys(&mut manifest, toml, "".to_string()),
        None => {}
    }
    if manifest.get_targets().len() == 0 {
        return Err(human(format!("either a [lib] or [[bin]] section must \
                                  be present")))
    }
    return Ok((manifest, paths));

    fn add_unused_keys(m: &mut Manifest, toml: &toml::Value, key: String) {
        match *toml {
            toml::Table(ref table) => {
                for (k, v) in table.iter() {
                    add_unused_keys(m, v, if key.len() == 0 {
                        k.clone()
                    } else {
                        key + "." + k.as_slice()
                    })
                }
            }
            toml::Array(ref arr) => {
                for v in arr.iter() {
                    add_unused_keys(m, v, key.clone());
                }
            }
            _ => m.add_warning(format!("unused manifest key: {}", key)),
        }
    }
}

pub fn parse(toml: &str, file: &Path) -> CargoResult<toml::TomlTable> {
    let mut parser = toml::Parser::new(toml.as_slice());
    match parser.parse() {
        Some(toml) => return Ok(toml),
        None => {}
    }
    let mut error_str = format!("could not parse input TOML\n");
    for error in parser.errors.iter() {
        let (loline, locol) = parser.to_linecol(error.lo);
        let (hiline, hicol) = parser.to_linecol(error.hi);
        error_str.push_str(format!("{}:{}:{}{} {}\n",
                                   file.display(),
                                   loline + 1, locol + 1,
                                   if loline != hiline || locol != hicol {
                                       format!("-{}:{}", hiline + 1,
                                               hicol + 1)
                                   } else {
                                       "".to_string()
                                   },
                                   error.desc).as_slice());
    }
    Err(human(error_str))
}

type TomlLibTarget = TomlTarget;
type TomlBinTarget = TomlTarget;
type TomlExampleTarget = TomlTarget;
type TomlTestTarget = TomlTarget;
type TomlBenchTarget = TomlTarget;

/*
 * TODO: Make all struct fields private
 */

#[deriving(Decodable)]
pub enum TomlDependency {
    SimpleDep(String),
    DetailedDep(DetailedTomlDependency)
}


#[deriving(Decodable, Clone, Default)]
pub struct DetailedTomlDependency {
    version: Option<String>,
    path: Option<String>,
    git: Option<String>,
    branch: Option<String>,
    tag: Option<String>,
    rev: Option<String>,
    features: Option<Vec<String>>,
    optional: Option<bool>,
    default_features: Option<bool>,
}

#[deriving(Decodable)]
pub struct TomlManifest {
    package: Option<Box<TomlProject>>,
    project: Option<Box<TomlProject>>,
    profile: Option<TomlProfiles>,
    lib: Option<ManyOrOne<TomlLibTarget>>,
    bin: Option<Vec<TomlBinTarget>>,
    example: Option<Vec<TomlExampleTarget>>,
    test: Option<Vec<TomlTestTarget>>,
    bench: Option<Vec<TomlTestTarget>>,
    dependencies: Option<HashMap<String, TomlDependency>>,
    dev_dependencies: Option<HashMap<String, TomlDependency>>,
    build_dependencies: Option<HashMap<String, TomlDependency>>,
    features: Option<HashMap<String, Vec<String>>>,
    target: Option<HashMap<String, TomlPlatform>>,
}

#[deriving(Decodable, Clone, Default)]
pub struct TomlProfiles {
    test: Option<TomlProfile>,
    doc: Option<TomlProfile>,
    bench: Option<TomlProfile>,
    dev: Option<TomlProfile>,
    release: Option<TomlProfile>,
}

#[deriving(Decodable, Clone, Default)]
pub struct TomlProfile {
    opt_level: Option<uint>,
    codegen_units: Option<uint>,
    debug: Option<bool>,
    rpath: Option<bool>,
}

#[deriving(Decodable)]
pub enum ManyOrOne<T> {
    Many(Vec<T>),
    One(T),
}

impl<T> ManyOrOne<T> {
    fn as_slice(&self) -> &[T] {
        match *self {
            Many(ref v) => v.as_slice(),
            One(ref t) => slice::ref_slice(t),
        }
    }
}

#[deriving(Decodable)]
pub struct TomlProject {
    name: String,
    version: TomlVersion,
    authors: Vec<String>,
    build: Option<TomlBuildCommandsList>,       // TODO: `String` instead
    links: Option<String>,
    exclude: Option<Vec<String>>,

    // package metadata
    description: Option<String>,
    homepage: Option<String>,
    documentation: Option<String>,
    readme: Option<String>,
    keywords: Option<Vec<String>>,
    license: Option<String>,
    repository: Option<String>,
}

// TODO: deprecated, remove
#[deriving(Decodable)]
pub enum TomlBuildCommandsList {
    SingleBuildCommand(String),
    MultipleBuildCommands(Vec<String>)
}

pub struct TomlVersion {
    version: semver::Version,
}

impl<E, D: Decoder<E>> Decodable<D, E> for TomlVersion {
    fn decode(d: &mut D) -> Result<TomlVersion, E> {
        let s = raw_try!(d.read_str());
        match s.as_slice().to_semver() {
            Ok(s) => Ok(TomlVersion { version: s }),
            Err(e) => Err(d.error(e.as_slice())),
        }
    }
}

impl TomlProject {
    pub fn to_package_id(&self, source_id: &SourceId) -> CargoResult<PackageId> {
        PackageId::new(self.name.as_slice(), self.version.version.clone(),
                       source_id)
    }
}

struct Context<'a> {
    deps: &'a mut Vec<Dependency>,
    source_id: &'a SourceId,
    nested_paths: &'a mut Vec<Path>
}

// These functions produce the equivalent of specific manifest entries. One
// wrinkle is that certain paths cannot be represented in the manifest due
// to Toml's UTF-8 requirement. This could, in theory, mean that certain
// otherwise acceptable executable names are not used when inside of
// `src/bin/*`, but it seems ok to not build executables with non-UTF8
// paths.
fn inferred_lib_target(name: &str, layout: &Layout) -> Vec<TomlTarget> {
    layout.lib.as_ref().map(|lib| {
        vec![TomlTarget {
            name: name.to_string(),
            path: Some(TomlPath(lib.clone())),
            .. TomlTarget::new()
        }]
    }).unwrap_or(Vec::new())
}

fn inferred_bin_targets(name: &str, layout: &Layout) -> Vec<TomlTarget> {
    layout.bins.iter().filter_map(|bin| {
        let name = if bin.as_vec() == b"src/main.rs" ||
                      *bin == layout.root.join("src/main.rs") {
            Some(name.to_string())
        } else {
            bin.filestem_str().map(|f| f.to_string())
        };

        name.map(|name| {
            TomlTarget {
                name: name,
                path: Some(TomlPath(bin.clone())),
                .. TomlTarget::new()
            }
        })
    }).collect()
}

fn inferred_example_targets(layout: &Layout) -> Vec<TomlTarget> {
    layout.examples.iter().filter_map(|ex| {
        ex.filestem_str().map(|name| {
            TomlTarget {
                name: name.to_string(),
                path: Some(TomlPath(ex.clone())),
                .. TomlTarget::new()
            }
        })
    }).collect()
}

fn inferred_test_targets(layout: &Layout) -> Vec<TomlTarget> {
    layout.tests.iter().filter_map(|ex| {
        ex.filestem_str().map(|name| {
            TomlTarget {
                name: name.to_string(),
                path: Some(TomlPath(ex.clone())),
                .. TomlTarget::new()
            }
        })
    }).collect()
}

fn inferred_bench_targets(layout: &Layout) -> Vec<TomlTarget> {
    layout.benches.iter().filter_map(|ex| {
        ex.filestem_str().map(|name| {
            TomlTarget {
                name: name.to_string(),
                path: Some(TomlPath(ex.clone())),
                .. TomlTarget::new()
            }
        })
    }).collect()
}

impl TomlManifest {
    pub fn to_manifest(&self, source_id: &SourceId, layout: &Layout)
        -> CargoResult<(Manifest, Vec<Path>)> {
        let mut nested_paths = vec!();

        let project = self.project.as_ref().or_else(|| self.package.as_ref());
        let project = try!(project.require(|| {
            human("No `package` or `project` section found.")
        }));

        let pkgid = try!(project.to_package_id(source_id));
        let metadata = pkgid.generate_metadata();

        // If we have no lib at all, use the inferred lib if available
        // If we have a lib with a path, we're done
        // If we have a lib with no path, use the inferred lib or_else package name

        let mut used_deprecated_lib = false;
        let lib = match self.lib {
            Some(ref libs) => {
                match *libs {
                    Many(..) => used_deprecated_lib = true,
                    _ => {}
                }
                libs.as_slice().iter().map(|t| {
                    if layout.lib.is_some() && t.path.is_none() {
                        TomlTarget {
                            path: layout.lib.as_ref().map(|p| TomlPath(p.clone())),
                            .. t.clone()
                        }
                    } else {
                        t.clone()
                    }
                }).collect()
            }
            None => inferred_lib_target(project.name.as_slice(), layout),
        };

        let bins = match self.bin {
            Some(ref bins) => {
                let bin = layout.main();

                bins.iter().map(|t| {
                    if bin.is_some() && t.path.is_none() {
                        TomlTarget {
                            path: bin.as_ref().map(|&p| TomlPath(p.clone())),
                            .. t.clone()
                        }
                    } else {
                        t.clone()
                    }
                }).collect()
            }
            None => inferred_bin_targets(project.name.as_slice(), layout)
        };

        let examples = match self.example {
            Some(ref examples) => examples.clone(),
            None => inferred_example_targets(layout),
        };

        let tests = match self.test {
            Some(ref tests) => tests.clone(),
            None => inferred_test_targets(layout),
        };

        let benches = if self.bench.is_none() || self.bench.as_ref().unwrap().is_empty() {
            inferred_bench_targets(layout)
        } else {
            self.bench.as_ref().unwrap().iter().map(|t| t.clone()).collect()
        };

        // processing the custom build script
        let (new_build, old_build) = match project.build {
            Some(SingleBuildCommand(ref cmd)) => {
                if cmd.as_slice().ends_with(".rs") && layout.root.join(cmd.as_slice()).exists() {
                    (Some(Path::new(cmd.as_slice())), Vec::new())
                } else {
                    (None, vec!(cmd.clone()))
                }
            },
            Some(MultipleBuildCommands(ref cmd)) => (None, cmd.clone()),
            None => (None, Vec::new())
        };

        // Get targets
        let profiles = self.profile.clone().unwrap_or(Default::default());
        let targets = normalize(lib.as_slice(),
                                bins.as_slice(),
                                new_build,
                                examples.as_slice(),
                                tests.as_slice(),
                                benches.as_slice(),
                                &metadata,
                                &profiles);

        if targets.is_empty() {
            debug!("manifest has no build targets");
        }

        let mut deps = Vec::new();

        {

            let mut cx = Context {
                deps: &mut deps,
                source_id: source_id,
                nested_paths: &mut nested_paths
            };

            // Collect the deps
            try!(process_dependencies(&mut cx, self.dependencies.as_ref(),
                                      |dep| dep));
            try!(process_dependencies(&mut cx, self.dev_dependencies.as_ref(),
                                      |dep| dep.kind(Development)));
            try!(process_dependencies(&mut cx, self.build_dependencies.as_ref(),
                                      |dep| dep.kind(Build)));

            if let Some(targets) = self.target.as_ref() {
                for (name, platform) in targets.iter() {
                    try!(process_dependencies(&mut cx,
                                              platform.dependencies.as_ref(),
                                              |dep| {
                        dep.only_for_platform(Some(name.clone()))
                    }));
                }
            }
        }

        let exclude = project.exclude.clone().unwrap_or(Vec::new());

        let has_old_build = old_build.len() >= 1;

        let summary = try!(Summary::new(pkgid, deps,
                                        self.features.clone()
                                            .unwrap_or(HashMap::new())));
        let metadata = ManifestMetadata {
            description: project.description.clone(),
            homepage: project.homepage.clone(),
            documentation: project.documentation.clone(),
            readme: project.readme.clone(),
            authors: project.authors.clone(),
            license: project.license.clone(),
            repository: project.repository.clone(),
            keywords: project.keywords.clone().unwrap_or(Vec::new()),
        };
        let mut manifest = Manifest::new(summary,
                                         targets,
                                         layout.root.join("target"),
                                         layout.root.join("doc"),
                                         old_build,
                                         exclude,
                                         project.links.clone(),
                                         metadata);
        if used_deprecated_lib {
            manifest.add_warning(format!("the [[lib]] section has been \
                                          deprecated in favor of [lib]"));
        }
        if has_old_build {
            manifest.add_warning(format!("warning: an arbitrary build command \
                                          has now been deprecated."));
            manifest.add_warning(format!("         It has been replaced by custom \
                                                   build scripts."));
            manifest.add_warning(format!("         For more information, see \
                                          http://doc.crates.io/build-script.html"));
        }
        Ok((manifest, nested_paths))
    }
}

fn process_dependencies<'a>(cx: &mut Context<'a>,
                            new_deps: Option<&HashMap<String, TomlDependency>>,
                            f: |Dependency| -> Dependency)
                            -> CargoResult<()> {
    let dependencies = match new_deps {
        Some(ref dependencies) => dependencies,
        None => return Ok(())
    };
    for (n, v) in dependencies.iter() {
        let details = match *v {
            SimpleDep(ref version) => {
                let mut d: DetailedTomlDependency = Default::default();
                d.version = Some(version.clone());
                d
            }
            DetailedDep(ref details) => details.clone(),
        };
        let reference = details.branch.clone()
            .or_else(|| details.tag.clone())
            .or_else(|| details.rev.clone())
            .unwrap_or_else(|| "master".to_string());

        let new_source_id = match details.git {
            Some(ref git) => {
                let loc = try!(git.as_slice().to_url().map_err(|e| {
                    human(e)
                }));
                Some(SourceId::for_git(&loc, reference.as_slice()))
            }
            None => {
                details.path.as_ref().map(|path| {
                    cx.nested_paths.push(Path::new(path.as_slice()));
                    cx.source_id.clone()
                })
            }
        }.unwrap_or(try!(SourceId::for_central()));

        let dep = try!(Dependency::parse(n.as_slice(),
                                         details.version.as_ref()
                                                .map(|v| v.as_slice()),
                                         &new_source_id));
        let dep = f(dep)
                     .features(details.features.unwrap_or(Vec::new()))
                     .default_features(details.default_features.unwrap_or(true))
                     .optional(details.optional.unwrap_or(false));
        cx.deps.push(dep);
    }

    Ok(())
}

#[deriving(Decodable, Show, Clone)]
struct TomlTarget {
    name: String,
    crate_type: Option<Vec<String>>,
    path: Option<TomlPathValue>,
    test: Option<bool>,
    doctest: Option<bool>,
    bench: Option<bool>,
    doc: Option<bool>,
    plugin: Option<bool>,
    harness: Option<bool>,
}

#[deriving(Decodable, Clone)]
enum TomlPathValue {
    TomlString(String),
    TomlPath(Path),
}

/// Corresponds to a `target` entry, but `TomlTarget` is already used.
#[deriving(Decodable)]
struct TomlPlatform {
    dependencies: Option<HashMap<String, TomlDependency>>,
}

impl TomlTarget {
    fn new() -> TomlTarget {
        TomlTarget {
            name: String::new(),
            crate_type: None,
            path: None,
            test: None,
            doctest: None,
            bench: None,
            doc: None,
            plugin: None,
            harness: None,
        }
    }
}

impl TomlPathValue {
    fn to_path(&self) -> Path {
        match *self {
            TomlString(ref s) => Path::new(s.as_slice()),
            TomlPath(ref p) => p.clone(),
        }
    }
}

impl fmt::Show for TomlPathValue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            TomlString(ref s) => s.fmt(f),
            TomlPath(ref p) => p.display().fmt(f),
        }
    }
}

fn normalize(libs: &[TomlLibTarget],
             bins: &[TomlBinTarget],
             custom_build: Option<Path>,
             examples: &[TomlExampleTarget],
             tests: &[TomlTestTarget],
             benches: &[TomlBenchTarget],
             metadata: &Metadata,
             profiles: &TomlProfiles) -> Vec<Target> {
    log!(4, "normalizing toml targets; lib={}; bin={}; example={}; test={}, benches={}",
         libs, bins, examples, tests, benches);

    enum TestDep { Needed, NotNeeded }

    fn merge(profile: Profile, toml: &Option<TomlProfile>) -> Profile {
        let toml = match *toml {
            Some(ref toml) => toml,
            None => return profile,
        };
        let opt_level = toml.opt_level.unwrap_or(profile.get_opt_level());
        let codegen_units = toml.codegen_units;
        let debug = toml.debug.unwrap_or(profile.get_debug());
        let rpath = toml.rpath.unwrap_or(profile.get_rpath());
        profile.opt_level(opt_level).codegen_units(codegen_units).debug(debug)
               .rpath(rpath)
    }

    fn target_profiles(target: &TomlTarget, profiles: &TomlProfiles,
                       dep: TestDep) -> Vec<Profile> {
        let mut ret = vec![
            merge(Profile::default_dev(), &profiles.dev),
            merge(Profile::default_release(), &profiles.release),
        ];

        match target.test {
            Some(true) | None => {
                ret.push(merge(Profile::default_test(), &profiles.test));
            }
            Some(false) => {}
        }

        let doctest = target.doctest.unwrap_or(true);
        match target.doc {
            Some(true) | None => {
                ret.push(merge(Profile::default_doc().doctest(doctest),
                               &profiles.doc));
            }
            Some(false) => {}
        }

        match target.bench {
            Some(true) | None => {
                ret.push(merge(Profile::default_bench(), &profiles.bench));
            }
            Some(false) => {}
        }

        match dep {
            Needed => {
                ret.push(merge(Profile::default_test().test(false),
                               &profiles.test));
                ret.push(merge(Profile::default_doc().doc(false),
                               &profiles.doc));
                ret.push(merge(Profile::default_bench().test(false),
                               &profiles.bench));
            }
            _ => {}
        }

        if target.plugin == Some(true) {
            ret = ret.into_iter().map(|p| p.for_host(true)).collect();
        }

        ret
    }

    fn lib_targets(dst: &mut Vec<Target>, libs: &[TomlLibTarget],
                   dep: TestDep, metadata: &Metadata, profiles: &TomlProfiles) {
        let l = &libs[0];
        let path = l.path.clone().unwrap_or_else(|| {
            TomlString(format!("src/{}.rs", l.name))
        });
        let crate_types = l.crate_type.clone().and_then(|kinds| {
            LibKind::from_strs(kinds).ok()
        }).unwrap_or_else(|| {
            vec![if l.plugin == Some(true) {Dylib} else {Lib}]
        });

        for profile in target_profiles(l, profiles, dep).iter() {
            let mut metadata = metadata.clone();
            // Libs and their tests are built in parallel, so we need to make
            // sure that their metadata is different.
            if profile.is_test() {
                metadata.mix(&"test");
            }
            dst.push(Target::lib_target(l.name.as_slice(), crate_types.clone(),
                                        &path.to_path(), profile,
                                        metadata));
        }
    }

    fn bin_targets(dst: &mut Vec<Target>, bins: &[TomlBinTarget],
                   dep: TestDep, metadata: &Metadata, profiles: &TomlProfiles,
                   default: |&TomlBinTarget| -> String) {
        for bin in bins.iter() {
            let path = bin.path.clone().unwrap_or_else(|| {
                TomlString(default(bin))
            });

            for profile in target_profiles(bin, profiles, dep).iter() {
                let metadata = if profile.is_test() {
                    // Make sure that the name of this test executable doesn't
                    // conflicts with a library that has the same name and is
                    // being tested
                    let mut metadata = metadata.clone();
                    metadata.mix(&format!("bin-{}", bin.name));
                    Some(metadata)
                } else {
                    None
                };
                dst.push(Target::bin_target(bin.name.as_slice(),
                                            &path.to_path(),
                                            profile,
                                            metadata));
            }
        }
    }

    fn custom_build_target(dst: &mut Vec<Target>, cmd: &Path,
                           profiles: &TomlProfiles) {
        let profiles = [
            merge(Profile::default_dev().for_host(true).custom_build(true),
                  &profiles.dev),
        ];

        let name = format!("build-script-{}", cmd.filestem_str().unwrap_or(""));

        for profile in profiles.iter() {
            dst.push(Target::custom_build_target(name.as_slice(),
                                                 cmd, profile, None));
        }
    }

    fn example_targets(dst: &mut Vec<Target>, examples: &[TomlExampleTarget],
                       profiles: &TomlProfiles,
                       default: |&TomlExampleTarget| -> String) {
        for ex in examples.iter() {
            let path = ex.path.clone().unwrap_or_else(|| TomlString(default(ex)));

            let profile = Profile::default_test().test(false);
            let profile = merge(profile, &profiles.test);
            dst.push(Target::example_target(ex.name.as_slice(),
                                            &path.to_path(),
                                            &profile));
        }
    }

    fn test_targets(dst: &mut Vec<Target>, tests: &[TomlTestTarget],
                    metadata: &Metadata, profiles: &TomlProfiles,
                    default: |&TomlTestTarget| -> String) {
        for test in tests.iter() {
            let path = test.path.clone().unwrap_or_else(|| {
                TomlString(default(test))
            });
            let harness = test.harness.unwrap_or(true);

            // make sure this metadata is different from any same-named libs.
            let mut metadata = metadata.clone();
            metadata.mix(&format!("test-{}", test.name));

            let profile = Profile::default_test().harness(harness);
            let profile = merge(profile, &profiles.test);
            dst.push(Target::test_target(test.name.as_slice(),
                                         &path.to_path(),
                                         &profile,
                                         metadata));
        }
    }

    fn bench_targets(dst: &mut Vec<Target>, benches: &[TomlBenchTarget],
                     metadata: &Metadata, profiles: &TomlProfiles,
                     default: |&TomlBenchTarget| -> String) {
        for bench in benches.iter() {
            let path = bench.path.clone().unwrap_or_else(|| {
                TomlString(default(bench))
            });
            let harness = bench.harness.unwrap_or(true);

            // make sure this metadata is different from any same-named libs.
            let mut metadata = metadata.clone();
            metadata.mix(&format!("bench-{}", bench.name));

            let profile = Profile::default_bench().harness(harness);
            let profile = merge(profile, &profiles.bench);
            dst.push(Target::bench_target(bench.name.as_slice(),
                                          &path.to_path(),
                                          &profile,
                                          metadata));
        }
    }

    let mut ret = Vec::new();

    let test_dep = if examples.len() > 0 || tests.len() > 0 || benches.len() > 0 {
        Needed
    } else {
        NotNeeded
    };

    match (libs, bins) {
        ([_, ..], [_, ..]) => {
            lib_targets(&mut ret, libs, Needed, metadata, profiles);
            bin_targets(&mut ret, bins, test_dep, metadata, profiles,
                        |bin| format!("src/bin/{}.rs", bin.name));
        },
        ([_, ..], []) => {
            lib_targets(&mut ret, libs, Needed, metadata, profiles);
        },
        ([], [_, ..]) => {
            bin_targets(&mut ret, bins, test_dep, metadata, profiles,
                        |bin| format!("src/{}.rs", bin.name));
        },
        ([], []) => ()
    }

    if let Some(custom_build) = custom_build {
        custom_build_target(&mut ret, &custom_build, profiles);
    }

    example_targets(&mut ret, examples, profiles,
                    |ex| format!("examples/{}.rs", ex.name));

    test_targets(&mut ret, tests, metadata, profiles,
                |test| {
                    if test.name.as_slice() == "test" {
                        "src/test.rs".to_string()
                    } else {
                        format!("tests/{}.rs", test.name)
                    }});

    bench_targets(&mut ret, benches, metadata, profiles,
                 |bench| {
                     if bench.name.as_slice() == "bench" {
                         "src/bench.rs".to_string()
                     } else {
                         format!("benches/{}.rs", bench.name)
                     }});

    ret
}

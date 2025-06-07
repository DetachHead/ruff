use anyhow::{Context, anyhow};
use ruff_db::files::{File, system_path_to_file};
use ruff_db::system::{SystemPath, SystemPathBuf, TestSystem};
use ty_project::pull_types_visitor::pull_types;
use ty_project::{ProjectDatabase, ProjectMetadata};

fn setup_db(project_root: &SystemPath, system: TestSystem) -> anyhow::Result<ProjectDatabase> {
    let project = ProjectMetadata::discover(project_root, &system)?;
    ProjectDatabase::new(project, system)
}

fn get_cargo_workspace_root() -> anyhow::Result<SystemPathBuf> {
    Ok(SystemPathBuf::from(String::from_utf8(
        std::process::Command::new("cargo")
            .args(["locate-project", "--workspace", "--message-format", "plain"])
            .output()?
            .stdout,
    )?)
    .parent()
    .unwrap()
    .to_owned())
}

/// Test that all snippets in testcorpus can be checked without panic (except for [`KNOWN_FAILURES`])
#[test]
fn corpus_no_panic() -> anyhow::Result<()> {
    let crate_root = String::from(env!("CARGO_MANIFEST_DIR"));
    run_corpus_tests(&format!("{crate_root}/resources/test/corpus/**/*.py"))
}

#[test]
fn parser_no_panic() -> anyhow::Result<()> {
    let workspace_root = get_cargo_workspace_root()?;
    run_corpus_tests(&format!(
        "{workspace_root}/crates/ruff_python_parser/resources/**/*.py"
    ))
}

#[test]
fn linter_af_no_panic() -> anyhow::Result<()> {
    let workspace_root = get_cargo_workspace_root()?;
    run_corpus_tests(&format!(
        "{workspace_root}/crates/ruff_linter/resources/test/fixtures/[a-f]*/**/*.py"
    ))
}

#[test]
fn linter_gz_no_panic() -> anyhow::Result<()> {
    let workspace_root = get_cargo_workspace_root()?;
    run_corpus_tests(&format!(
        "{workspace_root}/crates/ruff_linter/resources/test/fixtures/[g-z]*/**/*.py"
    ))
}

#[test]
fn linter_stubs_no_panic() -> anyhow::Result<()> {
    let workspace_root = get_cargo_workspace_root()?;
    run_corpus_tests(&format!(
        "{workspace_root}/crates/ruff_linter/resources/test/fixtures/**/*.pyi"
    ))
}

#[test]
fn typeshed_no_panic() -> anyhow::Result<()> {
    let workspace_root = get_cargo_workspace_root()?;
    run_corpus_tests(&format!(
        "{workspace_root}/crates/ty_vendored/vendor/typeshed/**/*.pyi"
    ))
}

#[expect(clippy::print_stdout)]
fn run_corpus_tests(pattern: &str) -> anyhow::Result<()> {
    let root = SystemPathBuf::from("/src");

    let system = TestSystem::default();
    let memory_fs = system.memory_file_system();
    memory_fs.create_directory_all(root.as_ref())?;

    let mut db = setup_db(&root, system.clone())?;

    let workspace_root = get_cargo_workspace_root()?;
    let workspace_root = workspace_root.to_string();

    let corpus = glob::glob(pattern).context("Failed to compile pattern")?;

    for path in corpus {
        let path = path.context("Failed to glob path")?;
        let path = SystemPathBuf::from_path_buf(path).map_err(|path| {
            anyhow!(
                "Failed to convert path '{path}' to system path",
                path = path.display()
            )
        })?;

        let relative_path = path.strip_prefix(&workspace_root)?;

        let (py_expected_to_fail, pyi_expected_to_fail) = KNOWN_FAILURES
            .iter()
            .find_map(|(path, py_fail, pyi_fail)| {
                if *path == relative_path.as_str().replace('\\', "/") {
                    Some((*py_fail, *pyi_fail))
                } else {
                    None
                }
            })
            .unwrap_or((false, false));

        let source = path.as_path();
        let source_filename = source.file_name().unwrap();

        let code = std::fs::read_to_string(source)?;

        let mut check_with_file_name = |path: &SystemPath| {
            if DO_NOT_ATTEMPT.contains(&&*relative_path.as_str().replace('\\', "/")) {
                println!("Skipping {relative_path:?} due to known stack overflow");
                return;
            }

            memory_fs.write_file_all(path, &code).unwrap();
            File::sync_path(&mut db, path);

            // this test is only asserting that we can pull every expression type without a panic
            // (and some non-expressions that clearly define a single type)
            let file = system_path_to_file(&db, path).unwrap();

            let result = std::panic::catch_unwind(|| pull_types(&db, file));

            let expected_to_fail = if path.extension().map(|e| e == "pyi").unwrap_or(false) {
                pyi_expected_to_fail
            } else {
                py_expected_to_fail
            };
            if let Err(err) = result {
                if !expected_to_fail {
                    println!(
                        "Check failed for {relative_path:?}. Consider fixing it or adding it to KNOWN_FAILURES"
                    );
                    std::panic::resume_unwind(err);
                }
            } else {
                assert!(
                    !expected_to_fail,
                    "Expected to panic, but did not. Consider removing this path from KNOWN_FAILURES"
                );
            }

            memory_fs.remove_file(path).unwrap();
            file.sync(&mut db);
        };

        if source.extension() == Some("pyi") {
            println!("checking {relative_path}");
            let pyi_dest = root.join(source_filename);
            check_with_file_name(&pyi_dest);
        } else {
            println!("checking {relative_path}");
            let py_dest = root.join(source_filename);
            check_with_file_name(&py_dest);

            let pyi_dest = root.join(format!("{source_filename}i"));
            println!("re-checking as stub file: {pyi_dest}");
            check_with_file_name(&pyi_dest);
        }
    }

    Ok(())
}

/// Whether or not the .py/.pyi version of this file is expected to fail
#[rustfmt::skip]
const KNOWN_FAILURES: &[(&str, bool, bool)] = &[
    // Fails with too-many-cycle-iterations due to a self-referential
    // type alias, see https://github.com/astral-sh/ty/issues/256
    ("crates/ruff_linter/resources/test/fixtures/pyflakes/F401_34.py", true, true),

    // These are all "expression should belong to this TypeInference region and TypeInferenceBuilder should have inferred a type for it"
    ("crates/ty_vendored/vendor/typeshed/stdlib/abc.pyi", true, true),
    ("crates/ty_vendored/vendor/typeshed/stdlib/builtins.pyi", true, true),
    ("crates/ty_vendored/vendor/typeshed/stdlib/curses/__init__.pyi", true, true),
];

/// Attempting to check one of these files causes a stack overflow
const DO_NOT_ATTEMPT: &[&str] = &[
    "crates/ty_vendored/vendor/typeshed/stdlib/pathlib/types.pyi",
    "crates/ty_vendored/vendor/typeshed/stdlib/types.pyi",
    "crates/ty_vendored/vendor/typeshed/stdlib/wsgiref/types.pyi",
];

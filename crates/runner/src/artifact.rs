use std::path::{Path, PathBuf};
use std::process::Command as Process;

use serde_json::Value;

use crate::plan::container_flag;
use crate::{Artifact, Error, Result};

/// A built app ready to install: the `.app` bundle or the `.apk`, and the id to launch it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub path: PathBuf,
    pub app_id: String,
}

/// Schemes of an Xcode workspace or project (`xcodebuild -list -json`). Blocking.
pub fn xcode_schemes(root: &Path, container: &Path) -> Result<Vec<String>> {
    let container_arg = container.display().to_string();
    let out = run(
        root,
        "xcodebuild",
        &["-list", "-json", container_flag(container), &container_arg],
    )?;
    parse_schemes(&out)
}

/// The scheme to build: the saved choice if it still exists, else the only scheme.
pub fn pick_scheme(container: &Path, schemes: &[String], saved: Option<&str>) -> Result<String> {
    if let Some(saved) = saved.filter(|s| schemes.iter().any(|x| x == s)) {
        return Ok(saved.to_owned());
    }
    match schemes {
        [] => Err(Error::NoScheme(container.display().to_string())),
        [only] => Ok(only.clone()),
        _ => Err(Error::SchemeRequired(container.display().to_string())),
    }
}

/// Finds the app a successful build produced. Blocking (Xcode asks `xcodebuild` for its build
/// settings, which takes a few seconds).
pub fn resolve_artifact(root: &Path, artifact: &Artifact) -> Result<Built> {
    match artifact {
        Artifact::Xcode {
            container,
            scheme,
            udid,
            build_dir,
        } => {
            let container_arg = container.display().to_string();
            let destination = format!("id={udid}");
            let build_dir = build_dir.display().to_string();
            let out = run(
                root,
                "xcodebuild",
                &[
                    "-showBuildSettings",
                    "-json",
                    container_flag(container),
                    &container_arg,
                    "-scheme",
                    scheme,
                    "-configuration",
                    "Debug",
                    "-destination",
                    &destination,
                    "-derivedDataPath",
                    &build_dir,
                ],
            )?;
            existing(parse_build_settings(&out)?)
        }
        Artifact::Gradle { dir, module } => {
            let outputs = root.join(dir).join(module).join("build/outputs/apk/debug");
            let metadata = outputs.join("output-metadata.json");
            let text = std::fs::read_to_string(&metadata)
                .map_err(|_| Error::ArtifactMissing(metadata.clone()))?;
            existing(parse_output_metadata(&text, &outputs)?)
        }
    }
}

fn existing(built: Built) -> Result<Built> {
    if built.path.exists() {
        Ok(built)
    } else {
        Err(Error::ArtifactMissing(built.path))
    }
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Result<String> {
    let out = Process::new(program).args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        return Err(Error::Command {
            command: format!("{program} {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn parse_json(what: &'static str, text: &str) -> Result<Value> {
    // xcodebuild can print warnings before the JSON.
    let start = text.find(['{', '[']).unwrap_or(0);
    serde_json::from_str(&text[start..]).map_err(|err| Error::Parse {
        what,
        detail: err.to_string(),
    })
}

fn parse_schemes(text: &str) -> Result<Vec<String>> {
    let json = parse_json("xcodebuild -list", text)?;
    let schemes = ["workspace", "project"]
        .iter()
        .find_map(|key| json.get(key)?.get("schemes")?.as_array())
        .ok_or_else(|| Error::Parse {
            what: "xcodebuild -list",
            detail: "no schemes".into(),
        })?;
    Ok(schemes
        .iter()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect())
}

/// The `.app` target of `xcodebuild -showBuildSettings -json` (test bundles are skipped).
fn parse_build_settings(text: &str) -> Result<Built> {
    let json = parse_json("xcodebuild -showBuildSettings", text)?;
    let missing = |detail: &str| Error::Parse {
        what: "xcodebuild -showBuildSettings",
        detail: detail.to_owned(),
    };
    let targets = json.as_array().ok_or_else(|| missing("not a list"))?;
    let settings = targets
        .iter()
        .filter_map(|t| t.get("buildSettings"))
        .find(|s| {
            s.get("WRAPPER_NAME")
                .and_then(Value::as_str)
                .is_some_and(|w| w.ends_with(".app"))
        })
        .ok_or_else(|| missing("no .app target"))?;
    let get = |key: &str| {
        settings
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| missing(key))
    };
    Ok(Built {
        path: Path::new(get("TARGET_BUILD_DIR")?).join(get("WRAPPER_NAME")?),
        app_id: get("PRODUCT_BUNDLE_IDENTIFIER")?.to_owned(),
    })
}

/// `output-metadata.json` written next to the APK by the Android Gradle plugin.
fn parse_output_metadata(text: &str, outputs: &Path) -> Result<Built> {
    let json = parse_json("output-metadata.json", text)?;
    let missing = |detail: &str| Error::Parse {
        what: "output-metadata.json",
        detail: detail.to_owned(),
    };
    let app_id = json
        .get("applicationId")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("applicationId"))?;
    let file = json
        .get("elements")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
        .and_then(|e| e.get("outputFile"))
        .and_then(Value::as_str)
        .ok_or_else(|| missing("elements[0].outputFile"))?;
    Ok(Built {
        path: outputs.join(file),
        app_id: app_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes_from_a_workspace_or_a_project() {
        let ws = r#"{"workspace":{"name":"Demo","schemes":["Demo","DemoTests"]}}"#;
        assert_eq!(parse_schemes(ws).unwrap(), vec!["Demo", "DemoTests"]);
        let proj = "Command line invocation:\n    xcodebuild -list\n\
                    {\"project\":{\"name\":\"Demo\",\"schemes\":[\"Demo\"],\"targets\":[\"Demo\"]}}";
        assert_eq!(parse_schemes(proj).unwrap(), vec!["Demo"]);
        assert!(parse_schemes("{}").is_err());
    }

    #[test]
    fn scheme_choice() {
        let c = Path::new("Demo.xcodeproj");
        let two = vec!["A".to_owned(), "B".to_owned()];
        assert_eq!(pick_scheme(c, &two, Some("B")).unwrap(), "B");
        assert!(matches!(
            pick_scheme(c, &two, Some("gone")),
            Err(Error::SchemeRequired(_))
        ));
        assert_eq!(pick_scheme(c, &two[..1], None).unwrap(), "A");
        assert!(matches!(pick_scheme(c, &[], None), Err(Error::NoScheme(_))));
    }

    #[test]
    fn app_target_from_build_settings() {
        let text = r#"[
          {"target":"DemoTests","buildSettings":{"WRAPPER_NAME":"DemoTests.xctest",
            "TARGET_BUILD_DIR":"/b/Debug-iphonesimulator","PRODUCT_BUNDLE_IDENTIFIER":"dev.demo.tests"}},
          {"target":"Demo","buildSettings":{"WRAPPER_NAME":"Demo.app",
            "TARGET_BUILD_DIR":"/b/Debug-iphonesimulator","PRODUCT_BUNDLE_IDENTIFIER":"dev.demo"}}
        ]"#;
        assert_eq!(
            parse_build_settings(text).unwrap(),
            Built {
                path: PathBuf::from("/b/Debug-iphonesimulator/Demo.app"),
                app_id: "dev.demo".into(),
            }
        );
        assert!(parse_build_settings("[]").is_err());
    }

    #[test]
    fn apk_from_output_metadata() {
        let text = r#"{"version":3,"artifactType":{"type":"APK","kind":"Directory"},
          "applicationId":"dev.demo","variantName":"debug",
          "elements":[{"type":"SINGLE","outputFile":"app-debug.apk"}]}"#;
        let outputs = Path::new("/w/app/build/outputs/apk/debug");
        assert_eq!(
            parse_output_metadata(text, outputs).unwrap(),
            Built {
                path: outputs.join("app-debug.apk"),
                app_id: "dev.demo".into(),
            }
        );
        assert!(parse_output_metadata(r#"{"applicationId":"x"}"#, outputs).is_err());
    }

    #[test]
    fn missing_gradle_output_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = Artifact::Gradle {
            dir: PathBuf::new(),
            module: "app".into(),
        };
        assert!(matches!(
            resolve_artifact(tmp.path(), &artifact),
            Err(Error::ArtifactMissing(_))
        ));
    }
}

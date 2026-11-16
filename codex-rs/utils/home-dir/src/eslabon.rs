//! eslabon: the Eslabon-mode home resolver.
//!
//! `lib.rs` stays literal upstream (`CODEX_HOME`, `~/.codex`, its validation
//! and its tests); this module adds only what Eslabon mode needs.

use codex_utils_absolute_path::AbsolutePathBuf;
use dirs::home_dir;
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;

/// Environment variable that overrides the home in Eslabon mode.
pub const ESLABON_HOME_ENV: &str = "ESLABON_HOME";
/// Default home folder, under the user's home directory.
pub const ESLABON_HOME_DIR_NAME: &str = ".eslabon";

const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// [`find_codex_home`](crate::find_codex_home) as Eslabon mode answers it:
/// `ESLABON_HOME`, otherwise `CODEX_HOME`, otherwise `~/.eslabon`; a home that
/// does not exist yet is created (`0700` on Unix).
// eslabon: called only from the `find_codex_home` hook when `modo_eslabon()`.
pub fn find_codex_home_eslabon() -> std::io::Result<AbsolutePathBuf> {
    let eslabon_home_env = std::env::var(ESLABON_HOME_ENV).ok();
    let codex_home_env = std::env::var(CODEX_HOME_ENV).ok();
    find_codex_home_desde(
        /*modo_eslabon*/ true,
        eslabon_home_env.as_deref(),
        codex_home_env.as_deref(),
        home_dir().as_deref(),
    )
}

/// [`find_codex_home`](crate::find_codex_home) over explicit values, so tests
/// never mutate process globals. Empty strings count as unset.
pub fn find_codex_home_desde(
    modo_eslabon: bool,
    eslabon_home: Option<&str>,
    codex_home: Option<&str>,
    home_usuario: Option<&Path>,
) -> std::io::Result<AbsolutePathBuf> {
    if !modo_eslabon {
        // eslabon: outside Eslabon mode upstream's validation decides, so both
        // answers stay identical without duplicating the rules.
        return super::find_codex_home_from_env(codex_home.filter(|val| !val.is_empty()));
    }
    let (path, variable) = resolver_home(
        /*modo_eslabon*/ true,
        eslabon_home,
        codex_home,
        home_usuario,
    )?;
    validar_home(path, variable.unwrap_or(ESLABON_HOME_DIR_NAME))
}

/// Pure home resolution: returns the chosen path and the environment variable
/// that supplied it (`None` for the default), or an error when no home exists.
///
/// eslabon: the single implementation behind `find_codex_home` and
/// `eslabon_config::home_por_defecto`; the default folder changes with the mode.
pub fn resolver_home(
    modo_eslabon: bool,
    eslabon_home: Option<&str>,
    codex_home: Option<&str>,
    home_usuario: Option<&Path>,
) -> std::io::Result<(PathBuf, Option<&'static str>)> {
    let eslabon_home = eslabon_home.filter(|val| !val.is_empty());
    let codex_home = codex_home.filter(|val| !val.is_empty());

    if modo_eslabon && let Some(val) = eslabon_home {
        return Ok((PathBuf::from(val), Some(ESLABON_HOME_ENV)));
    }
    if let Some(val) = codex_home {
        return Ok((PathBuf::from(val), Some(CODEX_HOME_ENV)));
    }

    let home = home_usuario.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not find home directory",
        )
    })?;
    let default_dir = if modo_eslabon {
        ESLABON_HOME_DIR_NAME
    } else {
        ".codex"
    };
    Ok((home.join(default_dir), None))
}

// eslabon: the home is private to its user; 0700 on Unix so other accounts on a
// shared machine cannot read the config or the auth payload.
fn crear_home(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Validation for a home chosen in Eslabon mode: a missing home is created, and
/// the path must end up being a canonicalized directory.
fn validar_home(path: PathBuf, variable: &str) -> std::io::Result<AbsolutePathBuf> {
    let val = path.to_string_lossy();
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            crear_home(&path).map_err(|err| {
                std::io::Error::new(
                    err.kind(),
                    format!("failed to create {variable} {val:?}: {err}"),
                )
            })?;
            std::fs::metadata(&path).map_err(|err| {
                std::io::Error::new(
                    err.kind(),
                    format!("failed to read {variable} {val:?}: {err}"),
                )
            })?
        }
        Err(err) => {
            return Err(std::io::Error::new(
                err.kind(),
                format!("failed to read {variable} {val:?}: {err}"),
            ));
        }
    };

    if !metadata.is_dir() {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{variable} points to {val:?}, but that path is not a directory"),
        ))
    } else {
        let canonical = path.canonicalize().map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("failed to canonicalize {variable} {val:?}: {err}"),
            )
        })?;
        AbsolutePathBuf::from_absolute_path(canonical)
    }
}

/// The home eslabon uses in Eslabon mode: `ESLABON_HOME` when set, otherwise
/// `CODEX_HOME` when set, otherwise `~/.eslabon`.
///
/// eslabon: reads the same live inputs as [`find_codex_home`](crate::find_codex_home)
/// but does not validate existence; `eslabon-config` uses it for its default home.
pub fn home_eslabon_bruto() -> Option<PathBuf> {
    let eslabon_home_env = std::env::var(ESLABON_HOME_ENV).ok();
    let codex_home_env = std::env::var(CODEX_HOME_ENV).ok();
    resolver_home(
        /*modo_eslabon*/ true,
        eslabon_home_env.as_deref(),
        codex_home_env.as_deref(),
        home_dir().as_deref(),
    )
    .ok()
    .map(|(path, _)| path)
}

/// The user's home directory as this crate resolves it (test helper shared by
/// eslabon-config, which must not depend on `dirs` directly).
pub fn home_dir_usuario() -> Option<PathBuf> {
    home_dir()
}

// eslabon: one shared detection of "Eslabon mode". The installed launcher
// marks the process with `ESLABON_LAUNCHER=1` (it runs the binary as `codex`
// so terminal managers recognize the agent), and argv[0] covers the `eslabon`
// binary itself. CLI and core read the answer only through this module.

/// Whether this process is running as Eslabon, reading the live process state.
pub fn modo_eslabon() -> bool {
    modo_eslabon_desde(
        std::env::var_os("ESLABON_LAUNCHER").as_deref(),
        std::env::args_os().next().as_deref(),
    )
}

/// [`modo_eslabon`] over explicit values, so tests never mutate process globals.
pub fn modo_eslabon_desde(launcher: Option<&OsStr>, argv0: Option<&OsStr>) -> bool {
    if launcher.is_some_and(|value| value == "1") {
        return true;
    }
    argv0.is_some_and(|arg0| {
        Path::new(arg0)
            .file_stem()
            .is_some_and(|stem| stem.eq_ignore_ascii_case("eslabon"))
    })
}

#[cfg(test)]
mod tests {
    use super::find_codex_home_desde;
    use super::modo_eslabon_desde;
    use super::resolver_home;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use std::ffi::OsStr;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::Path;
    use tempfile::TempDir;

    fn os(value: &str) -> &OsStr {
        OsStr::new(value)
    }

    fn expected(dir: &Path) -> AbsolutePathBuf {
        AbsolutePathBuf::from_absolute_path(dir.canonicalize().expect("canonicalize temp path"))
            .expect("absolute path")
    }

    #[test]
    fn resolver_home_table_in_eslabon_mode_prefers_eslabon_home() {
        let temp = TempDir::new().expect("temp home");
        let eslabon_dir = temp.path().join("home-eslabon");
        let codex_dir = temp.path().join("home-codex");
        fs::create_dir_all(&eslabon_dir).expect("eslabon dir");
        fs::create_dir_all(&codex_dir).expect("codex dir");

        struct Caso {
            modo: bool,
            eslabon: Option<&'static str>,
            codex: Option<&'static str>,
            esperado: &'static str,
            variable: Option<&'static str>,
        }

        // The four combinations in Eslabon mode plus the two outside it.
        let casos = [
            Caso {
                modo: true,
                eslabon: Some("home-eslabon"),
                codex: Some("home-codex"),
                esperado: "home-eslabon",
                variable: Some("ESLABON_HOME"),
            },
            Caso {
                modo: true,
                eslabon: Some("home-eslabon"),
                codex: None,
                esperado: "home-eslabon",
                variable: Some("ESLABON_HOME"),
            },
            Caso {
                modo: true,
                eslabon: None,
                codex: Some("home-codex"),
                esperado: "home-codex",
                variable: Some("CODEX_HOME"),
            },
            Caso {
                modo: true,
                eslabon: None,
                codex: None,
                esperado: ".eslabon",
                variable: None,
            },
            Caso {
                modo: false,
                eslabon: Some("home-eslabon"),
                codex: Some("home-codex"),
                esperado: "home-codex",
                variable: Some("CODEX_HOME"),
            },
            Caso {
                modo: false,
                eslabon: Some("home-eslabon"),
                codex: None,
                esperado: ".codex",
                variable: None,
            },
        ];

        for caso in casos {
            let eslabon = caso.eslabon.map(|rel| temp.path().join(rel));
            let codex = caso.codex.map(|rel| temp.path().join(rel));
            let eslabon_str = eslabon.as_ref().map(|path| path.to_str().expect("utf-8"));
            let codex_str = codex.as_ref().map(|path| path.to_str().expect("utf-8"));
            let (path, variable) =
                resolver_home(caso.modo, eslabon_str, codex_str, Some(temp.path()))
                    .expect("resolucion");
            assert_eq!(variable, caso.variable, "variable elegida");
            assert_eq!(path, temp.path().join(caso.esperado), "ruta elegida");
        }
    }

    #[test]
    fn empty_home_variants_count_as_unset() {
        assert_eq!(
            resolver_home(true, Some(""), Some(""), Some(Path::new("/home/usuario")))
                .expect("resolucion"),
            (Path::new("/home/usuario").join(".eslabon"), None)
        );
        assert_eq!(
            resolver_home(true, Some(""), None, Some(Path::new("/home/usuario")))
                .expect("resolucion"),
            (Path::new("/home/usuario").join(".eslabon"), None)
        );
        assert_eq!(
            resolver_home(false, Some(""), Some(""), Some(Path::new("/home/usuario")))
                .expect("resolucion"),
            (Path::new("/home/usuario").join(".codex"), None)
        );
    }

    #[test]
    fn missing_home_is_created_in_eslabon_mode_and_stays_fatal_outside() {
        let temp = TempDir::new().expect("temp home");
        // eslabon: in Eslabon mode the resolved home is created, so a first
        // run needs no manual `mkdir`.
        let missing_eslabon = temp.path().join("falta-eslabon");
        let missing_codex = temp.path().join("falta-codex");
        let resolved = find_codex_home_desde(
            /*modo_eslabon*/ true,
            Some(missing_eslabon.to_str().expect("utf-8")),
            Some(missing_codex.to_str().expect("utf-8")),
            Some(temp.path()),
        )
        .expect("missing ESLABON_HOME is created");
        assert_eq!(resolved, expected(&missing_eslabon));
        assert!(missing_eslabon.is_dir(), "home creado");
        assert!(
            !missing_codex.exists(),
            "CODEX_HOME no se usa en modo Eslabón"
        );

        let resolved = find_codex_home_desde(
            /*modo_eslabon*/ true,
            None,
            Some(missing_codex.to_str().expect("utf-8")),
            Some(temp.path()),
        )
        .expect("missing CODEX_HOME is created in Eslabon mode");
        assert_eq!(resolved, expected(&missing_codex));

        // The default `~/.eslabon` is created as well, with 0700 on Unix.
        let default_dir = temp.path().join(".eslabon");
        let resolved =
            find_codex_home_desde(/*modo_eslabon*/ true, None, None, Some(temp.path()))
                .expect("missing default home is created");
        assert_eq!(resolved, expected(&default_dir));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&default_dir)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "permisos del home creado");
        }

        // Outside Eslabon mode upstream's fatal error stays.
        let missing = temp.path().join("falta-fuera");
        let err = find_codex_home_desde(
            /*modo_eslabon*/ false,
            None,
            Some(missing.to_str().expect("utf-8")),
            Some(temp.path()),
        )
        .expect_err("outside Eslabon mode CODEX_HOME must exist");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(err.to_string().contains("CODEX_HOME"), "{err}");
        assert!(!missing.exists(), "fuera del modo no se crea nada");
    }

    #[test]
    fn file_path_is_fatal_and_names_the_variable() {
        let temp = TempDir::new().expect("temp home");
        let file_path = temp.path().join("no-es-directorio");
        fs::write(&file_path, "no").expect("write temp file");

        let err = find_codex_home_desde(
            /*modo_eslabon*/ true,
            None,
            Some(file_path.to_str().expect("utf-8")),
            Some(temp.path()),
        )
        .expect_err("file CODEX_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(err.to_string().contains("CODEX_HOME"), "{err}");
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[test]
    fn valid_directory_canonicalizes() {
        let temp = TempDir::new().expect("temp home");
        let resolved = find_codex_home_desde(
            /*modo_eslabon*/ true,
            Some(temp.path().to_str().expect("utf-8")),
            None,
            Some(temp.path()),
        )
        .expect("valid ESLABON_HOME");
        assert_eq!(resolved, expected(temp.path()));
    }

    #[test]
    fn without_home_dir_is_fatal() {
        let err = find_codex_home_desde(
            /*modo_eslabon*/ true, None, None, /*home_usuario*/ None,
        )
        .expect_err("no home dir");
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn launcher_marker_wins_over_argv0() {
        assert!(modo_eslabon_desde(Some(os("1")), Some(os("codex"))));
        assert!(modo_eslabon_desde(Some(os("1")), None));
        assert!(modo_eslabon_desde(Some(os("1")), Some(os("codex.exe"))));
    }

    #[test]
    fn argv0_named_eslabon_is_eslabon_mode() {
        assert!(modo_eslabon_desde(None, Some(os("eslabon"))));
        assert!(modo_eslabon_desde(None, Some(os("eslabon.exe"))));
        assert!(modo_eslabon_desde(None, Some(os("/usr/local/bin/eslabon"))));
        assert!(modo_eslabon_desde(
            None,
            Some(os("/home/usuario/.local/bin/eslabon"))
        ));
        assert!(modo_eslabon_desde(None, Some(os("ESLABON"))));
    }

    #[test]
    fn codex_is_not_eslabon_mode() {
        assert!(!modo_eslabon_desde(None, Some(os("codex"))));
        assert!(!modo_eslabon_desde(None, Some(os("/opt/codex/bin/codex"))));
        assert!(!modo_eslabon_desde(None, Some(os("codex.exe"))));
        assert!(!modo_eslabon_desde(None, None));
        // Only the marker value "1" means launcher.
        assert!(!modo_eslabon_desde(Some(os("0")), Some(os("codex"))));
        assert!(!modo_eslabon_desde(Some(os("true")), None));
    }
}

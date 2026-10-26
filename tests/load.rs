//! `[teams]` config from files and env vars.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Test helpers fail loudly.

use autumn_plugin_teams::config::TeamsConfig;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("teams-load-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn loads_base_profile_file_and_env_in_order() {
    let dir = temp_dir("order");
    std::fs::write(
        dir.join("autumn.toml"),
        r#"
[teams]
app_id_env = "BASE_ID"
invoke_timeout_ms = 1000

[teams.api]
max_attempts = 4

# Ignored: autumn strict config rejects this section.
[profile.prod.teams]
invoke_timeout_ms = 1500
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("autumn-prod.toml"),
        "[teams]\napp_password_env = \"PROD_PW\"\ninvoke_timeout_ms = 1200\n",
    )
    .unwrap();
    let env = vec![
        ("AUTUMN_TEAMS__API__MAX_ATTEMPTS".to_owned(), "5".to_owned()),
        (
            "AUTUMN_TEAMS__SERVICE_URL_HOSTS".to_owned(),
            "smba.trafficmanager.net, smba.infra.gov.teams.microsoft.us".to_owned(),
        ),
        ("OTHER".to_owned(), "x".to_owned()),
    ];
    let c = TeamsConfig::load_from_dir(&dir, &["prod".to_owned()], env).unwrap();
    assert_eq!(c.app_id_env, "BASE_ID");
    assert_eq!(c.invoke_timeout_ms, 1200);
    assert_eq!(c.app_password_env, "PROD_PW");
    assert_eq!(c.api.max_attempts, 5);
    assert_eq!(
        c.service_url_hosts,
        [
            "smba.trafficmanager.net",
            "smba.infra.gov.teams.microsoft.us"
        ]
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn no_files_gives_defaults() {
    let dir = temp_dir("empty");
    let c = TeamsConfig::load_from_dir(&dir, &[], Vec::new()).unwrap();
    assert_eq!(c, TeamsConfig::default());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bad_file_names_the_problem() {
    let dir = temp_dir("bad");
    std::fs::write(dir.join("autumn.toml"), "[teams]\nunknown_key = 1\n").unwrap();
    let e = TeamsConfig::load_from_dir(&dir, &[], Vec::new()).unwrap_err();
    assert!(e.to_string().contains("unknown_key"), "{e}");
    std::fs::remove_dir_all(dir).unwrap();
}

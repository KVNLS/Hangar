use super::*;

#[test]
fn run_config_round_trips_and_defaults() {
    let store = Store::open_in_memory().unwrap();
    let id = WorkspaceId::new();
    assert_eq!(run_config(&store, id), RunConfig::default());

    let config = RunConfig {
        port: Some(8082),
        scheme: Some("App".into()),
        stack: Some("expo".into()),
    };
    save_run_config(&store, id, &config).unwrap();
    assert_eq!(run_config(&store, id), config);
    assert_eq!(run_config(&store, WorkspaceId::new()), RunConfig::default());

    // An unreadable value runs with the defaults.
    let key: Setting<String> = Setting::scoped("run", id);
    store.set_setting(&key, &"nope".to_owned()).unwrap();
    assert_eq!(run_config(&store, id), RunConfig::default());
}

#[test]
fn a_picked_app_forgets_the_scheme() {
    let store = Store::open_in_memory().unwrap();
    let id = WorkspaceId::new();
    let saved = RunConfig {
        port: Some(8081),
        scheme: Some("Old".into()),
        stack: Some("xcode".into()),
    };
    save_run_config(&store, id, &saved).unwrap();

    save_run_choice(&store, id, RunChoice::Scheme("New".into())).unwrap();
    assert_eq!(
        run_config(&store, id),
        RunConfig {
            scheme: Some("New".into()),
            ..saved.clone()
        }
    );

    save_run_choice(&store, id, RunChoice::Stack("expo".into())).unwrap();
    assert_eq!(
        run_config(&store, id),
        RunConfig {
            port: Some(8081),
            scheme: None,
            stack: Some("expo".into()),
        }
    );
}

#[test]
fn forgetting_the_run_config_frees_the_port() {
    let store = Store::open_in_memory().unwrap();
    let id = WorkspaceId::new();
    let config = RunConfig {
        port: Some(8081),
        ..RunConfig::default()
    };
    save_run_config(&store, id, &config).unwrap();
    forget_run_config(&store, id).unwrap();
    assert_eq!(run_config(&store, id), RunConfig::default());
    // Forgetting nothing is fine.
    forget_run_config(&store, id).unwrap();
}

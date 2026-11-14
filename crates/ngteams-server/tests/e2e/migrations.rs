//! Both backends' migrations: up, down and up again, and a second start finding nothing to do.

use diesel_async::{AsyncConnection as _, AsyncMigrationHarness, AsyncPgConnection};
use diesel_migrations::FileBasedMigrations;
use ngteams_server::db::{Db, Sqlite};

use crate::support::{Backend, backends, database_url};

fn migrations(backend: Backend) -> FileBasedMigrations {
    let dir = match backend {
        Backend::Sqlite => "migrations/sqlite",
        Backend::Postgres => "migrations/postgres",
    };
    FileBasedMigrations::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/").to_owned() + dir).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_backends_migrations_go_up_down_and_up_again() {
    for backend in backends() {
        let dir = tempfile::tempdir().unwrap();
        let url = database_url(backend, &dir).await;
        let migrations = migrations(backend);
        tokio::task::spawn_blocking(move || match backend {
            Backend::Sqlite => {
                let path = url.strip_prefix("sqlite://").unwrap().to_owned();
                let connection = tokio::runtime::Handle::current().block_on(Sqlite::establish(&path)).unwrap();
                cycle(AsyncMigrationHarness::new(connection), &migrations);
            }
            Backend::Postgres => {
                let connection = tokio::runtime::Handle::current()
                    .block_on(AsyncPgConnection::establish(&url))
                    .unwrap();
                cycle(AsyncMigrationHarness::new(connection), &migrations);
            }
        })
        .await
        .unwrap();
    }
}

fn cycle<B: diesel::backend::Backend>(
    mut harness: impl diesel_migrations::MigrationHarness<B>,
    migrations: &FileBasedMigrations,
) {
    assert_eq!(harness.run_pending_migrations(migrations.clone()).unwrap().len(), 1);
    assert_eq!(harness.revert_all_migrations(migrations.clone()).unwrap().len(), 1);
    assert_eq!(harness.run_pending_migrations(migrations.clone()).unwrap().len(), 1);
    assert!(!harness.has_pending_migration(migrations.clone()).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_start_on_a_migrated_database_finds_nothing_to_do() {
    for backend in backends() {
        let dir = tempfile::tempdir().unwrap();
        let url = database_url(backend, &dir).await;
        let first = Db::connect(&url, 2).await.unwrap();
        let tenant = ngteams_server::db::meta::tenant(&first).await.unwrap();
        let second = Db::connect(&url, 2).await.unwrap();
        second.ping().await.unwrap();
        assert_eq!(ngteams_server::db::meta::tenant(&second).await.unwrap(), tenant, "{backend:?}");
        assert!(format!("{second:?}").contains(match backend {
            Backend::Sqlite => "sqlite",
            Backend::Postgres => "postgres",
        }));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_database_stops_the_node() {
    assert!(Db::connect("postgres://nobody@127.0.0.1:1/none", 1).await.is_err());
    assert!(Db::connect("sqlite:///nonexistent-dir/x/y.sqlite", 1).await.is_err());
}

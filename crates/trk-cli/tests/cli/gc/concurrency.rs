use super::*;

#[test]
fn gc_paused_launcher_close_allows_unrelated_writes_but_reserves_its_target() {
    for automatic in [false, true] {
        let fixture = GcEnv::new();
        check_gc_config(&fixture, "gc_grace = \"0\"");
        let (id, path) = fixture.closed("done", "term-target");
        let active = fixture.e.create(&["Unrelated active issue"]);
        fixture.claim(&active, None, "term-active", &fixture.host);
        let mut writer = fixture.store_at("2026-11-20T10:00:00Z");
        writer
            .conn()
            .busy_timeout(std::time::Duration::from_millis(100))
            .unwrap();
        let args = if automatic {
            vec!["check"]
        } else {
            vec!["gc", id.as_str()]
        };
        let mut runner = fixture.pause_gc(&args);

        // This BEGIN IMMEDIATE must finish while launcher close is still paused.
        writer
            .write("human:alice@example.com", false, |context| {
                context.note(&active, "written during launcher close", None, false)?;
                Ok(())
            })
            .unwrap();
        let error = writer
            .write("human:alice@example.com", false, |context| {
                context.reopen(&id, None)
            })
            .unwrap_err();
        assert!(
            matches!(
                &error,
                trk_core::error::Error::Coded(coded)
                    if coded.code == trk_core::error::Code::InvalidState
            ),
            "{error}"
        );
        assert!(error.to_string().contains("garbage collection"), "{error}");
        writer
            .write("human:alice@example.com", false, |context| {
                context.note(&active, "written after rejected reopen", None, false)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(fixture.e.ok(&["show", &id])["state"], "closed");
        assert!(fixture.closed_spaces().is_empty());
        assert!(path.is_dir());

        fixture.finish_gc(&mut runner);
        assert!(!path.exists());
        assert!(!fixture.branch_exists(&id));
        assert_eq!(fixture.closed_spaces(), ["target"]);
        let notes = fixture.notes(&active);
        assert!(
            notes
                .as_array()
                .unwrap()
                .iter()
                .any(|note| note["body"] == "written during launcher close")
        );
        assert!(
            notes
                .as_array()
                .unwrap()
                .iter()
                .any(|note| note["body"] == "written after rejected reopen")
        );
        assert_eq!(fixture.e.ok(&["reopen", &id])["state"], "open");
    }
}

#[test]
fn gc_revalidates_later_planned_issues_before_removing_their_resources() {
    for automatic in [false, true] {
        for reclaim in [false, true] {
            let fixture = GcEnv::new();
            check_gc_config(&fixture, "gc_grace = \"0\"");
            let mut ids = [
                fixture.e.create(&["First planned target"]),
                fixture.e.create(&["Later planned target"]),
            ];
            ids.sort();
            let first = &ids[0];
            let second = &ids[1];
            let first_path = fixture.worktree(first);
            let second_path = fixture.worktree(second);
            fixture.claim(first, Some(&first_path), "term-target", &fixture.host);
            fixture.claim(second, Some(&second_path), "term-history", &fixture.host);
            fixture.close_at(first, "done", "2026-11-20T10:00:00Z");
            fixture.close_at(second, "done", "2026-11-20T10:00:00Z");
            let mut writer = fixture.store_at("2026-11-20T10:00:00Z");
            writer
                .conn()
                .busy_timeout(std::time::Duration::from_millis(100))
                .unwrap();
            let args = if automatic {
                vec!["check"]
            } else {
                vec!["gc", first.as_str(), second.as_str()]
            };
            let mut runner = fixture.pause_gc(&args);

            writer
                .write("human:alice@example.com", false, |context| {
                    context.reopen(second, None)?;
                    if reclaim {
                        context.claim(
                            second,
                            trk_core::ops::ClaimOpts {
                                worktree: Some(second_path.to_string_lossy().into_owned()),
                                terminal: Some("term-history".into()),
                                host: Some(fixture.host.clone()),
                                launcher: Some("herdr".into()),
                                ..Default::default()
                            },
                        )?;
                    }
                    Ok(())
                })
                .unwrap();

            fixture.finish_gc(&mut runner);
            assert!(!first_path.exists());
            assert!(!fixture.branch_exists(first));
            assert!(second_path.is_dir());
            assert!(fixture.branch_exists(second));
            assert_eq!(fixture.closed_spaces(), ["target"]);
            assert!(!fixture.e.dir.path().join("closed-spaces/history").exists());
            assert_eq!(
                fixture.e.ok(&["show", second])["state"],
                if reclaim { "claimed" } else { "open" }
            );
        }
    }
}

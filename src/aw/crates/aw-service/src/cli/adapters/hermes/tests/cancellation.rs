use super::*;

#[test]
fn cancelled_installations_do_not_publish() {
    for already_cancelled in [true, false] {
        let original = if already_cancelled {
            "plugins: {enabled: [aw-native-hooks]}\nunknown: keep\n"
        } else {
            "unknown: keep\n"
        };
        let profile = Profile::new(original);
        let cancelled = AtomicBool::new(already_cancelled);
        let mut calls = 0;
        let error = install::install(&profile.0, &cancelled, |candidate, _, _| {
            calls += 1;
            fs::write(
                candidate.join("config.yaml"),
                "plugins: {enabled: [aw-native-hooks]}\nunknown: keep\n",
            )?;
            cancelled.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<aw_exec::Error>(),
            Some(aw_exec::Error::Cancelled)
        ));
        assert_eq!(calls, usize::from(!already_cancelled));
        let live = profile.0.join("config.yaml");
        assert_eq!(fs::read(&live).unwrap(), original.as_bytes());
        assert_eq!(
            fs::read_dir(&profile.0).unwrap().count(),
            if already_cancelled { 1 } else { 2 }
        );
        assert!(!profile.0.join("plugins").exists());
        let candidate = profile.0.join("candidate.yaml");
        fs::write(&candidate, "candidate: keep\n").unwrap();
        let backup = profile.0.join("backup.yaml");
        assert!(
            install::publish(&candidate, &live, original.as_bytes(), &backup, &cancelled)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert!(!install::displaced(&backup).exists());
        assert_eq!(fs::read(&live).unwrap(), original.as_bytes());
        fs::remove_file(candidate).unwrap();
        cancelled.store(false, Ordering::Release);
        install_profile(&profile.0).unwrap();
        assert!(install::installed(&profile.0).is_ok());
    }
}

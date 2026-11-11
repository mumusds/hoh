use super::*;

type R = Result<(), Box<dyn std::error::Error>>;

#[test]
fn an_object_is_kept_under_its_sha256_and_read_back_whole() -> R {
    let dir = tempfile::tempdir()?;
    let sha = put(dir.path(), b"hello")?;
    assert_eq!(
        sha,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    let path = object_path(dir.path(), &sha)?;
    assert_eq!(path, dir.path().join(".hoh/objects/sha256/2c").join(&sha));
    assert_eq!(std::fs::read(&path)?, b"hello");
    assert_eq!(get(dir.path(), &sha)?, b"hello");
    Ok(())
}

#[test]
fn the_same_content_twice_is_one_object_and_leaves_nothing_staged() -> R {
    let dir = tempfile::tempdir()?;
    let one = put(dir.path(), b"same")?;
    let two = put(dir.path(), b"same")?;
    assert_eq!(one, two);
    let shard = object_path(dir.path(), &one)?
        .parent()
        .ok_or("shard")?
        .to_owned();
    let names: Vec<String> = std::fs::read_dir(shard)?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names, [one]);
    Ok(())
}

#[test]
fn a_damaged_object_is_refused_on_reading_and_repaired_by_its_own_bytes() -> R {
    let dir = tempfile::tempdir()?;
    let sha = put(dir.path(), b"original")?;
    let path = object_path(dir.path(), &sha)?;
    std::fs::write(&path, b"tampered")?;
    let read = get(dir.path(), &sha);
    assert!(read.is_err(), "{read:?}");
    // Its name says what it must hold: the next write of those bytes puts them back.
    assert_eq!(put(dir.path(), b"original")?, sha);
    assert_eq!(get(dir.path(), &sha)?, b"original");
    let shard = path.parent().ok_or("shard")?;
    assert_eq!(std::fs::read_dir(shard)?.count(), 1, "no staged file left");
    Ok(())
}

#[test]
fn a_staged_file_a_crash_left_does_not_stand_in_the_way() -> R {
    let dir = tempfile::tempdir()?;
    let sha = sha256_hex(b"after a crash");
    let shard = object_path(dir.path(), &sha)?
        .parent()
        .ok_or("shard")?
        .to_owned();
    std::fs::create_dir_all(&shard)?;
    // Whatever a dead writer of the same process id left, under the next names this process would use.
    let next = STAGED.load(std::sync::atomic::Ordering::Relaxed);
    for n in next..next + 64 {
        std::fs::write(
            shard.join(format!(".{sha}.{}.{n}", std::process::id())),
            b"half",
        )?;
    }
    assert_eq!(put(dir.path(), b"after a crash")?, sha);
    assert_eq!(get(dir.path(), &sha)?, b"after a crash");
    Ok(())
}

#[test]
fn many_writers_of_the_same_bytes_at_once_all_get_one_whole_object() -> R {
    let dir = tempfile::tempdir()?;
    let bytes = vec![7u8; 1 << 20];
    let shas: Vec<String> = std::thread::scope(|scope| -> Result<Vec<String>, String> {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| put(dir.path(), &bytes)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().map_err(|_| "panicked".to_owned()).and_then(|r| r))
            .collect()
    })?;
    assert!(shas.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(get(dir.path(), &shas[0])?, bytes);
    let shard = object_path(dir.path(), &shas[0])?
        .parent()
        .ok_or("shard")?
        .to_owned();
    assert_eq!(std::fs::read_dir(shard)?.count(), 1, "no staged file left");
    Ok(())
}

#[test]
fn only_a_lower_case_sha256_names_an_object() -> R {
    let dir = tempfile::tempdir()?;
    for bad in [
        "",
        "../../etc/passwd",
        &"A".repeat(64),
        &"0".repeat(63),
        &format!("{}/", "0".repeat(63)),
    ] {
        assert!(object_path(dir.path(), bad).is_err(), "{bad}");
        assert!(get(dir.path(), bad).is_err(), "{bad}");
    }
    let missing = get(dir.path(), &"0".repeat(64));
    assert!(missing.is_err(), "{missing:?}");
    Ok(())
}

#[test]
fn the_resource_bundle_version_is_the_crate_version_and_a_digest_of_every_resource() {
    let v = resource_bundle_version();
    let (crate_version, digest) = v.split_once('+').unwrap_or_default();
    assert_eq!(crate_version, env!("CARGO_PKG_VERSION"));
    assert!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "{v}"
    );
    assert_eq!(v, resource_bundle_version());
    // The packing format is part of it: another format, another version.
    assert_eq!(
        digest_of(&with_format(&RESOURCES, crate::context::FORMAT)),
        digest
    );
    assert_ne!(digest_of(&with_format(&RESOURCES, "hoh-context/0")), digest);
    let paths: Vec<&str> = RESOURCES.iter().map(|(p, _)| *p).collect();
    for want in [
        "resources/prompts/commander.md",
        "resources/prompts/cto.md",
        "resources/prompts/developer.md",
        "resources/prompts/qa.md",
        "resources/prompts/researcher.md",
        "resources/prompts/reviewer.md",
        "contracts/protocol.schema.json",
        "contracts/independent-report.schema.json",
        "contracts/acceptance-observation.schema.json",
    ] {
        assert!(paths.contains(&want), "{want} not in the bundle");
    }
    // Order does not matter; any byte of any resource does.
    let a = [("x", "1"), ("y", "2")];
    let b = [("y", "2"), ("x", "1")];
    let c = [("x", "1"), ("y", "3")];
    assert_eq!(digest_of(&a), digest_of(&b));
    assert_ne!(digest_of(&a), digest_of(&c));
    // A path cannot be confused with content.
    assert_ne!(digest_of(&[("ab", "c")]), digest_of(&[("a", "bc")]));
}

#[test]
fn every_directory_on_the_way_to_an_object_is_synced_after_its_link() -> R {
    let dir = tempfile::tempdir()?;
    let synced = std::cell::RefCell::new(Vec::new());
    let sha = put_with(dir.path(), b"durable", &|d| {
        synced.borrow_mut().push(d.to_owned());
        Ok(())
    })?;
    let shard = object_path(dir.path(), &sha)?
        .parent()
        .ok_or("shard")?
        .to_owned();
    let objects = dir.path().join(".hoh/objects");
    assert_eq!(
        synced.into_inner(),
        [
            shard,
            objects.join("sha256"),
            objects.clone(),
            dir.path().join(".hoh")
        ],
        "the object's own directory first, then each parent whose entry names the next"
    );
    // A sync that fails fails the put: the caller must not name an object that may not outlive a power loss.
    let failed = put_with(dir.path(), b"durable", &|_| {
        Err(std::io::Error::other("no sync"))
    });
    assert!(failed.is_err(), "{failed:?}");
    Ok(())
}

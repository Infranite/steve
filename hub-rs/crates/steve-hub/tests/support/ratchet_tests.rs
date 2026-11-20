use super::ratchet::{self, Counts, Mode};
use std::{fs, panic::catch_unwind};

fn counts(n: usize) -> Counts {
    Counts::from([(vec!["fixture.rs".into()], n)])
}

#[test]
fn unchanged_passes_and_shrink_requires_explicit_bless() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("inventory.txt");
    fs::write(&file, "# fixture\nfixture.rs\t2\n").unwrap();
    ratchet::check_at(counts(2), &file, 1, "# fixture\n", "fix it", Mode::Check);
    assert!(
        catch_unwind(|| ratchet::check_at(
            counts(1),
            &file,
            1,
            "# fixture\n",
            "fix it",
            Mode::Check
        ))
        .is_err()
    );
    assert_eq!(ratchet::read(&file, 1), counts(2));
    ratchet::check_at(counts(1), &file, 1, "# fixture\n", "fix it", Mode::Bless);
    assert_eq!(ratchet::read(&file, 1), counts(1));
    assert!(
        catch_unwind(|| ratchet::check_at(
            Counts::new(),
            &file,
            1,
            "# fixture\n",
            "fix it",
            Mode::Check
        ))
        .is_err()
    );
    ratchet::check_at(
        Counts::new(),
        &file,
        1,
        "# fixture\n",
        "fix it",
        Mode::Bless,
    );
    assert_eq!(fs::read_to_string(file).unwrap(), "# fixture\n");
}

#[test]
fn growth_and_new_keys_are_rejected_even_with_bless_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("inventory.txt");
    let text = "fixture.rs\t1\n";
    fs::write(&file, text).unwrap();
    for now in [counts(2), Counts::from([(vec!["new.rs".into()], 1)])] {
        for mode in [Mode::Check, Mode::Bless] {
            assert!(
                catch_unwind(|| ratchet::check_at(now.clone(), &file, 1, "", "fix it", mode))
                    .is_err()
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), text);
        }
    }
}

#[test]
fn a_missing_inventory_needs_explicit_initialization() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("inventory.txt");
    assert!(
        catch_unwind(|| ratchet::check_at(counts(1), &file, 1, "", "fix it", Mode::Check)).is_err()
    );
    assert!(!file.exists());
    assert!(
        catch_unwind(|| ratchet::check_at(counts(1), &file, 1, "", "fix it", Mode::Bless)).is_err()
    );
    assert!(!file.exists());
    ratchet::check_at(counts(1), &file, 1, "", "fix it", Mode::Init);
    assert_eq!(ratchet::read(&file, 1), counts(1));
    let before = fs::read_to_string(&file).unwrap();
    assert!(
        catch_unwind(|| ratchet::check_at(counts(99), &file, 1, "", "fix it", Mode::Init)).is_err()
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
    fs::remove_file(&file).unwrap();
    assert!(
        catch_unwind(|| ratchet::check_at(counts(99), &file, 1, "", "fix it", Mode::Bless))
            .is_err()
    );
    assert!(!file.exists());
}

#[test]
fn malformed_inventories_cannot_be_blessed_away() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("inventory.txt");
    for text in [
        "f\t0\n",
        "f\tx\n",
        "f\t1\nf\t2\n",
        "f\n",
        "f\tx\t1\n",
        "\t1\n",
    ] {
        fs::write(&file, text).unwrap();
        assert!(
            catch_unwind(|| ratchet::check_at(counts(1), &file, 1, "", "fix it", Mode::Bless))
                .is_err(),
            "{text}"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), text);
    }
}

#[test]
fn switches_are_explicit_and_mutually_exclusive() {
    assert_eq!(Mode::from_switches(None, None), Mode::Check);
    assert_eq!(Mode::from_switches(Some("0"), Some("yes")), Mode::Check);
    assert_eq!(Mode::from_switches(Some("1"), None), Mode::Bless);
    assert_eq!(Mode::from_switches(None, Some("1")), Mode::Init);
    assert!(catch_unwind(|| Mode::from_switches(Some("1"), Some("1"))).is_err());
}

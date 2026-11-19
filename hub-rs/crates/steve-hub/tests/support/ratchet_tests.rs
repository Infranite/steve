use super::ratchet::{self, Counts};
use std::{fs, panic::catch_unwind};

fn counts(n: usize) -> Counts {
    Counts::from([(vec!["fixture.rs".into()], n)])
}

#[test]
fn unchanged_passes_and_shrink_requires_explicit_bless() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("inventory.txt");
    fs::write(&file, "# fixture\nfixture.rs\t2\n").unwrap();
    ratchet::check_at(counts(2), &file, 1, "# fixture\n", "fix it", false);
    assert!(
        catch_unwind(|| ratchet::check_at(counts(1), &file, 1, "# fixture\n", "fix it", false))
            .is_err()
    );
    assert_eq!(ratchet::read(&file, 1), counts(2));
    ratchet::check_at(counts(1), &file, 1, "# fixture\n", "fix it", true);
    assert_eq!(ratchet::read(&file, 1), counts(1));
    assert!(
        catch_unwind(|| ratchet::check_at(Counts::new(), &file, 1, "# fixture\n", "fix it", false))
            .is_err()
    );
    ratchet::check_at(Counts::new(), &file, 1, "# fixture\n", "fix it", true);
    assert_eq!(fs::read_to_string(file).unwrap(), "# fixture\n");
}

#[test]
fn growth_and_new_keys_are_rejected_even_with_bless_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("inventory.txt");
    let text = "fixture.rs\t1\n";
    fs::write(&file, text).unwrap();
    for now in [counts(2), Counts::from([(vec!["new.rs".into()], 1)])] {
        for bless in [false, true] {
            assert!(
                catch_unwind(|| ratchet::check_at(now.clone(), &file, 1, "", "fix it", bless))
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
    assert!(catch_unwind(|| ratchet::check_at(counts(1), &file, 1, "", "fix it", false)).is_err());
    assert!(!file.exists());
    ratchet::check_at(counts(1), &file, 1, "", "fix it", true);
    assert_eq!(ratchet::read(&file, 1), counts(1));
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
            catch_unwind(|| ratchet::check_at(counts(1), &file, 1, "", "fix it", true)).is_err(),
            "{text}"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), text);
    }
}

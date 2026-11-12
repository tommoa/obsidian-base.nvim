use std::{collections::BTreeMap, fs, path::Path};

use obsidian_base_worker::{
    index::{Index, Overlay},
    limits::Limits,
};
use tempfile::TempDir;

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn build(root: &Path, overlays: &BTreeMap<String, Overlay>) -> Index {
    Index::build(
        root.to_owned(),
        overlays,
        &Default::default(),
        Limits::default(),
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn backlinks_preserve_relative_precedence_and_natural_basename_fallback() {
    let root = TempDir::new().unwrap();
    for path in [
        "Folder10/Target.md",
        "Folder02/Target.md",
        "Folder2/Target.md",
        "From/Target.md",
        "Elsewhere/Only.md",
        "From/image.png",
        "Elsewhere/image.pdf",
    ] {
        write(root.path(), path, "");
    }
    write(
        root.path(),
        "From/Host.md",
        "[[Target]] [[Target.md#heading|alias]] [[../Folder10/Target]] \
         [[../Folder2/./Target.md]] [[missing/Target]] [[Only]] \
         [[Missing]] [[target]] [[image.png]]",
    );
    write(root.path(), "Other.md", "[[Target]] [[image]]");
    let index = build(root.path(), &BTreeMap::new());

    for (link, expected) in [
        ("Target", Some("From/Target.md")),
        ("Target.md", Some("From/Target.md")),
        ("../Folder10/Target", Some("Folder10/Target.md")),
        ("../Folder2/./Target.md", Some("Folder2/Target.md")),
        ("..\\Folder10\\Target.md", Some("Folder10/Target.md")),
        ("missing/Target", Some("Folder2/Target.md")),
        ("Only", Some("Elsewhere/Only.md")),
        ("Missing", None),
        ("target", None),
        ("image.png", Some("From/image.png")),
        ("image", Some("Elsewhere/image.pdf")),
    ] {
        assert_eq!(
            index.resolve_link(link, "From/Host.md").as_deref(),
            expected,
            "{link}"
        );
    }
    assert_eq!(
        index.records()["From/Target.md"].backlinks,
        ["From/Host.md"; 2]
    );
    assert_eq!(
        index.records()["Folder10/Target.md"].backlinks,
        ["From/Host.md"]
    );
    assert_eq!(
        index.records()["Folder2/Target.md"].backlinks,
        ["From/Host.md", "From/Host.md", "Other.md"]
    );
    assert!(index.records()["Folder02/Target.md"].backlinks.is_empty());
    assert_eq!(
        index.records()["Elsewhere/Only.md"].backlinks,
        ["From/Host.md"]
    );
    assert_eq!(
        index.records()["From/image.png"].backlinks,
        ["From/Host.md"]
    );
    assert_eq!(
        index.records()["Elsewhere/image.pdf"].backlinks,
        ["Other.md"]
    );
}

#[test]
fn backlink_winners_include_overlay_only_records_and_change_with_each_build() {
    let root = TempDir::new().unwrap();
    write(root.path(), "Folder10/Target.md", "");
    write(root.path(), "Host.md", "[[Target]]");
    let overlays = BTreeMap::from([(
        "Folder2/Target.md".to_owned(),
        Overlay {
            contents: "[[Host]]".to_owned(),
            created_at: 0,
        },
    )]);
    let index = build(root.path(), &overlays);
    assert_eq!(index.records()["Folder2/Target.md"].backlinks, ["Host.md"]);
    assert!(index.records()["Folder10/Target.md"].backlinks.is_empty());
    assert_eq!(index.records()["Host.md"].backlinks, ["Folder2/Target.md"]);

    let index = build(root.path(), &BTreeMap::new());
    assert_eq!(index.records()["Folder10/Target.md"].backlinks, ["Host.md"]);
    assert!(index.records()["Host.md"].backlinks.is_empty());
}

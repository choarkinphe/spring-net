use super::*;

fn empty() -> SaveRequest {
    SaveRequest {
        schema_version: 3,
        expected_revision: 0,
        sites: vec![],
        applications: vec![],
        groups: vec![],
        tags: vec![],
        tag_assignments: vec![],
        resource_supplements: vec![],
    }
}
fn site(id: &str, url: &str) -> Site {
    site_from_old(id, id, url, "").unwrap()
}
fn app(id: &str, kind: ApplicationKind, refs: &[&str]) -> Application {
    Application {
        id: id.into(),
        name: id.into(),
        kind,
        category: ApplicationCategory::Custom,
        provider: String::new(),
        product: String::new(),
        aliases: vec![],
        access_type: String::new(),
        site_ids: refs.iter().map(|s| s.to_string()).collect(),
        reference_rules: vec![],
    }
}
fn tag(id: &str, name: &str) -> Tag {
    Tag {
        id: id.into(),
        name: name.into(),
    }
}
fn assignment(id: &str, tags: &[&str]) -> TagAssignment {
    TagAssignment {
        resource_id: id.into(),
        tag_ids: tags.iter().map(|t| t.to_string()).collect(),
    }
}
fn group(id: &str, tags: &[&str]) -> GroupInput {
    GroupInput {
        id: id.into(),
        name: id.into(),
        description: String::new(),
        tag_ids: tags.iter().map(|t| t.to_string()).collect(),
    }
}
fn request(snapshot: Snapshot) -> SaveRequest {
    SaveRequest {
        schema_version: 3,
        expected_revision: snapshot.revision,
        sites: snapshot
            .sites
            .into_iter()
            .filter(|s| !s.builtin)
            .map(|s| s.site)
            .collect(),
        applications: snapshot
            .applications
            .into_iter()
            .filter(|a| !a.builtin)
            .map(|a| a.application)
            .collect(),
        groups: snapshot
            .groups
            .into_iter()
            .filter(|g| !g.builtin)
            .map(|g| GroupInput {
                id: g.group.id,
                name: g.group.name,
                description: g.group.description,
                tag_ids: g.group.tag_ids,
            })
            .collect(),
        tags: snapshot
            .tags
            .into_iter()
            .filter(|t| !t.builtin)
            .map(|t| t.tag)
            .collect(),
        tag_assignments: snapshot.tag_assignments,
        resource_supplements: snapshot.resource_supplements,
    }
}
fn lists() -> Vec<SiteList> {
    storage::load_presets(&Path::new(env!("CARGO_MANIFEST_DIR")).join("presets")).unwrap()
}

#[test]
fn strict_save_dtos_reject_derived_fields_old_version_and_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), &[]).unwrap();
    let mut old = empty();
    old.schema_version = 2;
    assert!(manager.save(old).unwrap_err().contains("schema_version=3"));
    let base = serde_json::to_value(empty()).unwrap();
    for key in ["tags", "tag_assignments", "resource_supplements"] {
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<SaveRequest>(missing).is_err());
    }
    let mut value = serde_json::to_value(group("g", &[])).unwrap();
    value["application_ids"] = serde_json::json!([]);
    assert!(serde_json::from_value::<GroupInput>(value).is_err());
    for (key, value) in [
        ("builtin", serde_json::json!(true)),
        ("source", serde_json::json!("official")),
        ("tag_ids", serde_json::json!([])),
    ] {
        let mut a = serde_json::to_value(app("a", ApplicationKind::Application, &[])).unwrap();
        a[key] = value;
        assert!(serde_json::from_value::<Application>(a).is_err());
    }
    assert!(!dir.path().join(FILE_NAME).exists());
}

#[test]
fn tags_or_rename_delete_last_tag_and_preset_assignment() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), &lists()).unwrap();
    let preset = manager.snapshot().tags[0].tag.id.clone();
    let builtin = manager.snapshot().applications[0].application.id.clone();
    let builtin_group = manager.snapshot().groups[0].group.id.clone();
    let mut r = empty();
    r.applications = vec![
        app("a", ApplicationKind::Application, &[]),
        app("w", ApplicationKind::Website, &[]),
        app("untagged", ApplicationKind::Website, &[]),
    ];
    r.tags = vec![tag("t", "  personal  "), tag("u", "second")];
    r.tag_assignments = vec![
        assignment("a", &["t", "u", &preset]),
        assignment("w", &["u"]),
        assignment(&builtin, &["t"]),
    ];
    r.groups = vec![group("or", &["t", "u"]), group("empty", &[])];
    let s = manager.save(r).unwrap();
    assert_eq!(
        s.tags.iter().find(|t| t.tag.id == "t").unwrap().tag.name,
        "personal"
    );
    let g = s.groups.iter().find(|g| g.group.id == "or").unwrap();
    assert_eq!(g.group.application_ids.len(), 3);
    assert!(g.group.application_ids.contains(&builtin));
    assert!(
        s.groups
            .iter()
            .find(|g| g.group.id == builtin_group)
            .unwrap()
            .group
            .application_ids
            .contains(&"a".into())
    );
    assert!(
        s.groups
            .iter()
            .find(|g| g.group.id == "empty")
            .unwrap()
            .group
            .application_ids
            .is_empty()
    );
    assert!(
        s.applications
            .iter()
            .find(|a| a.application.id == "untagged")
            .unwrap()
            .tag_ids
            .is_empty()
    );
    let mut r = request(s);
    r.tags[0].name = "renamed".into();
    let s = manager.save(r).unwrap();
    assert!(
        s.tags
            .iter()
            .any(|t| t.tag.id == "t" && t.tag.name == "renamed")
    );
    let mut r = request(s);
    r.tags.clear();
    for a in &mut r.tag_assignments {
        a.tag_ids.retain(|id| id == &preset);
    }
    for g in &mut r.groups {
        g.tag_ids.clear();
    }
    let s = manager.save(r).unwrap();
    assert!(
        s.groups
            .iter()
            .filter(|g| !g.builtin)
            .all(|g| g.group.application_ids.is_empty())
    );
    assert_eq!(
        Manager::new(dir.path(), &lists())
            .unwrap()
            .snapshot()
            .revision,
        3
    );
}

#[test]
fn tag_validation_and_custom_namespace_are_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let m = Manager::new(dir.path(), &lists()).unwrap();
    let builtin_tag = m.snapshot().tags[0].tag.clone();
    let mut cases = vec![];
    let mut r = empty();
    r.tags = vec![tag("t", "x"), tag("u", " X ")];
    cases.push(r);
    let mut r = empty();
    r.tags = vec![tag("personal", &m.snapshot().tags[0].tag.name)];
    cases.push(r);
    let mut r = empty();
    r.tags = vec![tag("t", "  ")];
    cases.push(r);
    let mut r = empty();
    r.tags = vec![builtin_tag];
    cases.push(r);
    let mut r = empty();
    r.tags = vec![tag("builtin-spoof", "x")];
    cases.push(r);
    let mut r = empty();
    r.tags = vec![tag("t", "x")];
    r.applications = vec![app("t", ApplicationKind::Application, &[])];
    cases.push(r);
    let mut r = empty();
    r.tags = vec![tag("t", "x")];
    r.groups = vec![group("g", &["t", "t"])];
    cases.push(r);
    let mut r = empty();
    r.groups = vec![group("g", &["missing"])];
    cases.push(r);
    let mut r = empty();
    r.tag_assignments = vec![assignment("missing", &[])];
    cases.push(r);
    let mut r = empty();
    r.tags = (0..257)
        .map(|i| tag(&format!("t{i}"), &format!("t{i}")))
        .collect();
    cases.push(r);
    let mut r = empty();
    r.tags = (0..101)
        .map(|i| tag(&format!("t{i}"), &format!("t{i}")))
        .collect();
    r.groups = vec![GroupInput {
        tag_ids: r.tags.iter().map(|t| t.id.clone()).collect(),
        ..group("g", &[])
    }];
    cases.push(r);
    for r in cases {
        assert!(m.save(r).is_err());
    }
    assert_eq!(m.snapshot().revision, 0);
}

#[test]
fn websites_allow_cross_origin_and_no_endpoint_but_block_shared_invalid_edits() {
    let dir = tempfile::tempdir().unwrap();
    let m = Manager::new(dir.path(), &[]).unwrap();
    let mut r = empty();
    r.sites = vec![
        site("s", "https://login.example/path"),
        site("cdn", "http://cdn.other.test/api"),
    ];
    r.applications = vec![
        app("w", ApplicationKind::Website, &["s", "cdn"]),
        app("a", ApplicationKind::Application, &["s"]),
        app("empty", ApplicationKind::Website, &[]),
    ];
    let s = m.save(r).unwrap();
    let mut r = request(s);
    r.sites[0] = site("s", "unknown.example");
    assert!(m.save(r.clone()).is_err());
    r.applications[0].kind = ApplicationKind::Application;
    assert!(m.save(r).is_ok());
}

#[test]
fn builtin_supplements_are_personal_bounded_and_never_override_official_data() {
    let dir = tempfile::tempdir().unwrap();
    let m = Manager::new(dir.path(), &lists()).unwrap();
    let before = m.snapshot();
    let b = &before.applications[0];
    let bid = b.application.id.clone();
    let official = b.application.clone();
    let mut r = empty();
    r.sites = vec![site("login", "https://other.example/login")];
    r.resource_supplements = vec![ResourceSupplement {
        resource_id: bid.clone(),
        site_ids: vec!["login".into()],
        reference_rules: vec![],
    }];
    let s = m.save(r).unwrap();
    let a = s
        .applications
        .iter()
        .find(|a| a.application.id == bid)
        .unwrap();
    assert_eq!(a.application.name, official.name);
    assert_eq!(a.application.kind, official.kind);
    assert_eq!(
        &a.application.site_ids[..official.site_ids.len()],
        official.site_ids.as_slice()
    );
    assert_eq!(a.supplement.as_ref().unwrap().site_ids, ["login"]);
    let mut r = request(s);
    r.resource_supplements[0].site_ids = official.site_ids.clone();
    assert!(m.save(r).is_err());
    let mut r = request(m.snapshot());
    r.sites[0] = site("login", "unknown.example");
    assert!(m.save(r).is_err());
    let mut r = request(m.snapshot());
    r.resource_supplements[0].resource_id = "not-builtin".into();
    assert!(m.save(r).is_err());
    let mut r = request(m.snapshot());
    r.sites = (0..100)
        .map(|i| site(&format!("s{i}"), "https://example.test/"))
        .collect();
    r.resource_supplements[0].site_ids = r.sites.iter().map(|s| s.id.clone()).collect();
    assert!(m.save(r).is_err());
    let mut r = request(m.snapshot());
    r.resource_supplements.clear();
    let s = m.save(r).unwrap();
    assert_eq!(
        s.applications
            .iter()
            .find(|a| a.application.id == bid)
            .unwrap()
            .application,
        official
    );
    assert!(s.sites.iter().any(|s| s.site.id == "login")); // No implicit orphan deletion.
}

#[test]
fn supplements_cannot_reuse_official_rule_ids_or_sources() {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let presets = crate::connections::load(&root.join("presets/connections")).unwrap();
    let m = Manager::new_with_connections(dir.path(), &lists(), &presets).unwrap();
    let s = m.snapshot();
    let b = s
        .applications
        .iter()
        .find(|a| !a.application.reference_rules.is_empty())
        .unwrap();
    let mut rule = b.application.reference_rules[0].clone();
    rule.source_ids.clear();
    let mut r = empty();
    r.resource_supplements = vec![ResourceSupplement {
        resource_id: b.application.id.clone(),
        site_ids: vec![],
        reference_rules: vec![rule],
    }];
    assert!(m.save(r.clone()).is_err());
    r.resource_supplements[0].reference_rules[0].id = "personal-rule".into();
    r.resource_supplements[0].reference_rules[0].source_ids = vec!["official".into()];
    assert!(m.save(r.clone()).is_err());
    r.resource_supplements[0].reference_rules[0]
        .source_ids
        .clear();
    assert!(m.save(r).is_ok());
}

fn v2_app(id: &str, category: &str, refs: &[&str]) -> serde_json::Value {
    serde_json::json!({"id":id,"name":id,"category":category,"provider":"","product":"","aliases":[],"access_type":"","site_ids":refs,"reference_rules":[]})
}
#[test]
fn v2_migration_keeps_bytes_ids_revision_members_unknowns_and_stable_dedicated_tags() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let old = serde_json::json!({"version":2,"revision":9,"sites":[site("http","https://example.test/page"),site("unknown","unknown.example")],
        "applications":[v2_app("web","web",&["http"]),v2_app("mixed","web",&["http","unknown"]),v2_app("empty","web",&[]),v2_app("gds","gds",&["http"])],
        "groups":[{"id":"g1","name":"same","description":"","application_ids":["mixed","web"]},{"id":"g2","name":"same","description":"","application_ids":["mixed"]},{"id":"g3","name":"empty","description":"","application_ids":[]}]});
    let bytes = serde_json::to_vec_pretty(&old).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let m = Manager::new(dir.path(), &[]).unwrap();
    let s = m.snapshot();
    assert_eq!(s.revision, 9);
    assert_eq!(s.version, 3);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        s.applications
            .iter()
            .find(|a| a.application.id == "web")
            .unwrap()
            .application
            .kind,
        ApplicationKind::Website
    );
    for id in ["mixed", "empty", "gds"] {
        assert_eq!(
            s.applications
                .iter()
                .find(|a| a.application.id == id)
                .unwrap()
                .application
                .kind,
            ApplicationKind::Application
        );
    }
    let mixed = s
        .applications
        .iter()
        .find(|a| a.application.id == "mixed")
        .unwrap();
    assert_eq!(mixed.application.site_ids, ["http", "unknown"]);
    assert!(!mixed.limitations.is_empty());
    for (i, expected) in [(0, vec!["web", "mixed"]), (1, vec!["mixed"]), (2, vec![])] {
        assert_eq!(s.groups[i].group.application_ids, expected);
    }
    assert_ne!(s.groups[0].group.tag_ids, s.groups[1].group.tag_ids);
    assert_eq!(
        serde_json::to_value(&s).unwrap(),
        serde_json::to_value(Manager::new(dir.path(), &[]).unwrap().snapshot()).unwrap()
    );
    m.save(request(s)).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["version"], 3);
    assert!(value["groups"][0].get("application_ids").is_none());
    assert_eq!(
        Manager::new(dir.path(), &[]).unwrap().snapshot().revision,
        10
    );
}

#[test]
fn builtin_website_classification_is_not_inferred_from_https_gds_urls() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let presets = crate::connections::load(&root.join("presets/connections")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let s = Manager::new_with_connections(dir.path(), &lists(), &presets)
        .unwrap()
        .snapshot();
    assert_eq!(
        s.applications
            .iter()
            .filter(|a| a.application.kind == ApplicationKind::Website)
            .count(),
        55
    );
    assert!(
        s.applications
            .iter()
            .filter(|a| a.application.category != ApplicationCategory::Web)
            .all(|a| a.application.kind == ApplicationKind::Application)
    );
}

#[test]
fn migration_accepts_maximal_legal_v2_membership_budget_without_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let mut apps = vec![];
    let mut groups = vec![];
    // v2 10,000 refs entirely spent on memberships; migrated assignment and projection
    // overhead must not make this legal source fail or silently drop members.
    for i in 0..1000 {
        apps.push(v2_app(&format!("a{i}"), "custom", &[]));
    }
    for i in 0..10 {
        groups.push(serde_json::json!({"id":format!("g{i}"),"name":format!("g{i}"),"description":"","application_ids":(0..1000).map(|j|format!("a{j}")).collect::<Vec<_>>()}));
    }
    let old = serde_json::json!({"version":2,"revision":5,"sites":[],"applications":apps,"groups":groups});
    let bytes = serde_json::to_vec(&old).unwrap();
    assert!(bytes.len() < MAX_BYTES);
    std::fs::write(dir.path().join(FILE_NAME), &bytes).unwrap();
    let s = Manager::new(dir.path(), &[]).unwrap().snapshot();
    assert_eq!(
        s.groups
            .iter()
            .map(|g| g.group.application_ids.len())
            .sum::<usize>(),
        10_000
    );
    assert_eq!(s.tag_assignments.len(), 1000);
    assert_eq!(std::fs::read(dir.path().join(FILE_NAME)).unwrap(), bytes);
}

#[test]
fn migration_generated_ids_and_casefolded_names_never_shadow_old_data() {
    let dir = tempfile::tempdir().unwrap();
    let lists = lists();
    let baseline = Manager::new(dir.path(), &lists).unwrap().snapshot();
    let name = baseline.tags[0].tag.name.to_uppercase();
    let occupied = stable_id("legacy-tag", &["g1"]);
    let old = serde_json::json!({"version":2,"revision":1,"sites":[],"applications":[v2_app(&occupied,"custom",&[])],"groups":[
        {"id":"g1","name":name,"description":"","application_ids":[occupied]},
        {"id":"g2","name":format!("{} (1)",name),"description":"","application_ids":[]},
        {"id":"g3","name":name,"description":"","application_ids":[]}
    ]});
    std::fs::write(
        dir.path().join(FILE_NAME),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    let s = Manager::new(dir.path(), &lists).unwrap().snapshot();
    let tags: Vec<_> = s.tags.iter().filter(|t| !t.builtin).collect();
    assert_eq!(tags.len(), 3);
    assert!(
        tags.iter()
            .all(|t| t.tag.id != occupied && t.tag.name.len() <= 200)
    );
    let names: HashSet<_> = s.tags.iter().map(|t| t.tag.name.to_lowercase()).collect();
    assert_eq!(names.len(), s.tags.len());
    assert_eq!(
        s.groups
            .iter()
            .find(|g| g.group.id == "g1")
            .unwrap()
            .group
            .application_ids,
        vec![occupied]
    );
}

#[test]
fn checked_aggregate_reference_budget_is_enforced() {
    let mut total = 250_000usize;
    assert!(v3::add_refs(&mut total, 1).is_err());
    let mut total = usize::MAX;
    assert!(v3::add_refs(&mut total, 1).is_err());
}

#[test]
fn exhausted_revision_and_final_json_limit_leave_source_and_memory_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let value = serde_json::json!({"version":3,"revision":u64::MAX,"sites":[],"applications":[],"groups":[],"tags":[],"tag_assignments":[],"resource_supplements":[]});
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let m = Manager::new(dir.path(), &[]).unwrap();
    let r = request(m.snapshot());
    assert!(m.save(r).unwrap_err().contains("耗尽"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

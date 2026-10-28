//! V3 writable DTOs, read-only migration, overlay validation and runtime projection.
use super::*;

const MAX_TAGS: usize = 256;
// A legal v2 catalog can spend its entire 10,000-reference budget on membership.
// Migration adds <=10,000 assignments + 50 group refs + the same membership projection.
// Include all v3 tag refs, builtin membership expansion and endpoint/rule refs, without truncation.
const MAX_TOTAL_REFS: usize = 250_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationKind {
    Website,
    Application,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tag {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct CatalogTag {
    #[serde(flatten)]
    pub tag: Tag,
    pub builtin: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagAssignment {
    pub resource_id: String,
    pub tag_ids: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupInput {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tag_ids: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSupplement {
    pub resource_id: String,
    pub site_ids: Vec<String>,
    pub reference_rules: Vec<ReferenceRule>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct V3Catalog {
    pub version: u32,
    pub revision: u64,
    pub sites: Vec<Site>,
    pub applications: Vec<Application>,
    pub groups: Vec<GroupInput>,
    pub tags: Vec<Tag>,
    pub tag_assignments: Vec<TagAssignment>,
    pub resource_supplements: Vec<ResourceSupplement>,
}

pub(super) fn builtin_tags(groups: &[CatalogGroup]) -> Vec<CatalogTag> {
    groups
        .iter()
        .map(|g| CatalogTag {
            tag: Tag {
                id: stable_id("builtin-tag", &[&g.group.id]),
                name: g.group.name.clone(),
            },
            builtin: true,
        })
        .collect()
}
pub(super) fn prepare_builtins(
    apps: &mut [CatalogApplication],
    groups: &mut [CatalogGroup],
) -> Vec<CatalogTag> {
    let tags = builtin_tags(groups);
    for (g, t) in groups.iter_mut().zip(&tags) {
        g.group.tag_ids = vec![t.tag.id.clone()];
        for a in apps
            .iter_mut()
            .filter(|a| g.group.application_ids.contains(&a.application.id))
        {
            a.tag_ids.push(t.tag.id.clone());
        }
    }
    tags
}

/// Strict old shape first: neither kind nor derived/tag/provenance fields belong in v2.
pub(super) fn decode_v2(bytes: &[u8]) -> Result<SavedCatalog, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OldApplication {
        id: String,
        name: String,
        category: ApplicationCategory,
        provider: String,
        product: String,
        aliases: Vec<String>,
        access_type: String,
        site_ids: Vec<String>,
        reference_rules: Vec<ReferenceRule>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OldGroup {
        id: String,
        name: String,
        description: String,
        application_ids: Vec<String>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OldCatalog {
        version: u32,
        revision: u64,
        sites: Vec<Site>,
        applications: Vec<OldApplication>,
        groups: Vec<OldGroup>,
    }
    let old: OldCatalog =
        serde_json::from_slice(bytes).map_err(|e| format!("v2 应用目录损坏，未覆盖原文件：{e}"))?;
    Ok(SavedCatalog {
        version: old.version,
        revision: old.revision,
        sites: old.sites,
        applications: old
            .applications
            .into_iter()
            .map(|a| Application {
                id: a.id,
                name: a.name,
                kind: ApplicationKind::Application,
                category: a.category,
                provider: a.provider,
                product: a.product,
                aliases: a.aliases,
                access_type: a.access_type,
                site_ids: a.site_ids,
                reference_rules: a.reference_rules,
            })
            .collect(),
        groups: old
            .groups
            .into_iter()
            .map(|g| Group {
                id: g.id,
                name: g.name,
                description: g.description,
                tag_ids: vec![],
                application_ids: g.application_ids,
            })
            .collect(),
    })
}

pub(super) fn migrate(
    mut old: SavedCatalog,
    sites: &[CatalogSite],
    apps: &[CatalogApplication],
    builtin_tags: &[CatalogTag],
) -> Result<V3Catalog, String> {
    let endpoints: HashMap<_, _> = sites
        .iter()
        .map(|s| (&s.site.id, &s.site))
        .chain(old.sites.iter().map(|s| (&s.id, s)))
        .collect();
    for a in &mut old.applications {
        a.kind = if a.category == ApplicationCategory::Web
            && !a.site_ids.is_empty()
            && a.site_ids
                .iter()
                .all(|id| endpoints.get(id).is_some_and(|s| website_site(s).is_ok()))
        {
            ApplicationKind::Website
        } else {
            ApplicationKind::Application
        };
    }
    let mut tags = vec![];
    let mut groups = vec![];
    let mut assignments: Vec<TagAssignment> = vec![];
    let mut names: HashSet<String> = builtin_tags
        .iter()
        .map(|t| t.tag.name.trim().to_lowercase())
        .collect();
    let mut reserved: HashSet<String> = old
        .sites
        .iter()
        .map(|s| s.id.clone())
        .chain(old.applications.iter().map(|a| a.id.clone()))
        .chain(
            old.applications
                .iter()
                .flat_map(|a| a.reference_rules.iter().map(|r| r.id.clone())),
        )
        .chain(old.groups.iter().map(|g| g.id.clone()))
        .collect();
    for g in old.groups {
        let mut tag_id = stable_id("legacy-tag", &[&g.id]);
        let mut collision = 0usize;
        while !reserved.insert(tag_id.clone()) {
            collision = collision.checked_add(1).ok_or("旧分组标签 ID 计数溢出")?;
            tag_id = stable_id("legacy-tag", &[&g.id, &collision.to_string()]);
        }
        // Same-named/case-variant/builtin-named groups remain independent. The bounded
        // suffix loop must not reject an otherwise legal old catalog's literal names.
        let base = g.name.trim();
        let mut name = base.to_owned();
        let mut ordinal = 0usize;
        while !names.insert(name.to_lowercase()) {
            ordinal = ordinal.checked_add(1).ok_or("旧分组标签名称计数溢出")?;
            let suffix = format!(" ({ordinal})");
            let limit = 200usize.saturating_sub(suffix.len());
            let mut end = base.len().min(limit);
            while !base.is_char_boundary(end) {
                end -= 1;
            }
            name = format!("{}{}", &base[..end], suffix);
        }
        tags.push(Tag {
            id: tag_id.clone(),
            name,
        });
        for resource_id in &g.application_ids {
            if let Some(a) = assignments
                .iter_mut()
                .find(|a| &a.resource_id == resource_id)
            {
                a.tag_ids.push(tag_id.clone());
            } else {
                assignments.push(TagAssignment {
                    resource_id: resource_id.clone(),
                    tag_ids: vec![tag_id.clone()],
                });
            }
        }
        groups.push(GroupInput {
            id: g.id,
            name: g.name,
            description: g.description,
            tag_ids: vec![tag_id.clone()],
        });
        let actual: HashSet<_> = apps
            .iter()
            .map(|a| &a.application.id)
            .chain(old.applications.iter().map(|a| &a.id))
            .filter(|id| {
                assignments
                    .iter()
                    .any(|a| &a.resource_id == *id && a.tag_ids.contains(&tag_id))
            })
            .cloned()
            .collect();
        if actual != g.application_ids.into_iter().collect() {
            return Err("旧组成员迁移不一致，未覆盖原文件".into());
        }
    }
    Ok(V3Catalog {
        version: VERSION,
        revision: old.revision,
        sites: old.sites,
        applications: old.applications,
        groups,
        tags,
        tag_assignments: assignments,
        resource_supplements: vec![],
    })
}

pub(super) fn normalize_tags(saved: &mut V3Catalog) -> Result<(), String> {
    for t in &mut saved.tags {
        t.name = t.name.trim().to_owned();
    }
    Ok(())
}
pub(super) fn add_refs(total: &mut usize, n: usize) -> Result<(), String> {
    *total = total.checked_add(n).ok_or("目录引用总数溢出")?;
    if *total > MAX_TOTAL_REFS {
        return Err("目录引用总数不能超过 250000".into());
    }
    Ok(())
}
fn tag_refs(refs: &[String], tags: &HashSet<&str>, total: &mut usize) -> Result<(), String> {
    if refs.len() > MAX_ITEMS {
        return Err("每个资源/分组最多 100 个标签".into());
    }
    let mut seen = HashSet::new();
    for id in refs {
        if !tags.contains(id.as_str()) || !seen.insert(id) {
            return Err("标签引用不存在或重复".into());
        }
    }
    add_refs(total, refs.len())
}
fn website_site(site: &Site) -> Result<(), String> {
    if site.transport != Transport::Tcp
        || !matches!(site.protocol, Protocol::Http | Protocol::Https)
    {
        return Err("网站连接必须是明确 HTTP(S) 协议及 TCP 传输".into());
    }
    validate_site(site)
}

pub(super) fn validate(
    saved: &V3Catalog,
    builtin_sites: &[CatalogSite],
    builtin_apps: &[CatalogApplication],
    builtin_groups: &[CatalogGroup],
    builtin_tags: &[CatalogTag],
) -> Result<(), String> {
    if saved.version != VERSION {
        return Err("只接受 v3 目录".into());
    }
    if saved.sites.len() > MAX_SITES
        || saved.applications.len() > MAX_APPLICATIONS
        || saved.groups.len() > MAX_GROUPS
        || saved.tags.len() > MAX_TAGS
    {
        return Err("自定义端点/资源最多 1000 个，分组最多 50 个，用户标签最多 256 个".into());
    }
    let mut ids: HashSet<String> = builtin_sites
        .iter()
        .map(|s| s.site.id.clone())
        .chain(builtin_apps.iter().map(|a| a.application.id.clone()))
        .chain(
            builtin_apps
                .iter()
                .flat_map(|a| a.application.reference_rules.iter().map(|r| r.id.clone())),
        )
        .chain(builtin_groups.iter().map(|g| g.group.id.clone()))
        .chain(builtin_tags.iter().map(|t| t.tag.id.clone()))
        .collect();
    let mut total = 0;
    for a in builtin_apps {
        if a.application.site_ids.len() > MAX_ITEMS
            || a.application.reference_rules.len() > MAX_ITEMS
        {
            return Err("单资源有效端点/规则各最多 100 条".into());
        }
        add_refs(&mut total, a.application.site_ids.len())?;
        add_refs(&mut total, a.application.reference_rules.len())?;
    }
    for g in builtin_groups {
        add_refs(&mut total, g.group.tag_ids.len())?;
    }
    let mut names: HashSet<String> = builtin_tags
        .iter()
        .map(|t| t.tag.name.trim().to_lowercase())
        .collect();
    for t in &saved.tags {
        custom_id(&t.id, &mut ids, "标签")?;
        text(&t.name, 200, true, "标签名称")?;
        if t.name != t.name.trim() || !names.insert(t.name.to_lowercase()) {
            return Err("标签名称必须 trim 且不能重复".into());
        }
    }
    let tag_ids: HashSet<&str> = builtin_tags
        .iter()
        .map(|t| t.tag.id.as_str())
        .chain(saved.tags.iter().map(|t| t.id.as_str()))
        .collect();
    let mut sites: HashMap<&str, &Site> = builtin_sites
        .iter()
        .map(|s| (s.site.id.as_str(), &s.site))
        .collect();
    for s in &saved.sites {
        custom_id(&s.id, &mut ids, "端点")?;
        validate_site(s)?;
        sites.insert(&s.id, s);
    }
    let mut resources: HashMap<&str, &Application> = builtin_apps
        .iter()
        .map(|a| (a.application.id.as_str(), &a.application))
        .collect();
    for a in &saved.applications {
        custom_id(&a.id, &mut ids, "资源")?;
        text(&a.name, 200, true, "资源名称")?;
        text(&a.provider, 200, false, "厂商")?;
        text(&a.product, 200, false, "产品")?;
        text(&a.access_type, 100, false, "接入方式")?;
        if a.aliases.len() > 20 {
            return Err("资源别名最多 20 条".into());
        }
        let mut seen = HashSet::new();
        for alias in &a.aliases {
            text(alias, 200, true, "资源别名")?;
            if !seen.insert(alias) {
                return Err("资源别名不能重复".into());
            }
        }
        resource_refs(a, &sites, &mut ids, &mut total)?;
        resources.insert(&a.id, a);
    }
    let mut assigned = HashSet::new();
    for a in &saved.tag_assignments {
        if !resources.contains_key(a.resource_id.as_str()) || !assigned.insert(&a.resource_id) {
            return Err("标签分配资源不存在或重复".into());
        }
        tag_refs(&a.tag_ids, &tag_ids, &mut total)?;
    }
    for g in &saved.groups {
        custom_id(&g.id, &mut ids, "分组")?;
        text(&g.name, 200, true, "分组名称")?;
        text(&g.description, 1000, false, "分组说明")?;
        tag_refs(&g.tag_ids, &tag_ids, &mut total)?;
    }
    if saved.resource_supplements.len() > builtin_apps.len() {
        return Err("个人补充条目超过内置资源数".into());
    }
    let mut supplemented = HashSet::new();
    for s in &saved.resource_supplements {
        let a = builtin_apps
            .iter()
            .find(|a| a.application.id == s.resource_id)
            .ok_or("个人补充只能关联内置资源")?;
        if !supplemented.insert(&s.resource_id) {
            return Err("同一资源个人补充重复".into());
        }
        if s.site_ids
            .iter()
            .any(|id| !saved.sites.iter().any(|s| &s.id == id))
        {
            return Err("个人补充只允许引用自定义端点".into());
        }
        let mut effective = a.application.clone();
        effective.site_ids.extend(s.site_ids.clone());
        effective.reference_rules.extend(s.reference_rules.clone());
        // Validate only personal rule IDs, but cap the full effective endpoint/rule lists.
        if effective.site_ids.len() > MAX_ITEMS || effective.reference_rules.len() > MAX_ITEMS {
            return Err("单资源有效端点/规则各最多 100 条".into());
        }
        let personal = Application {
            site_ids: s.site_ids.clone(),
            reference_rules: s.reference_rules.clone(),
            ..a.application.clone()
        };
        resource_refs(&personal, &sites, &mut ids, &mut total)?;
    }
    // Bound effective tags and derived membership too, before producing any runtime snapshot.
    for a in resources.values() {
        let base = builtin_apps
            .iter()
            .find(|b| b.application.id == a.id)
            .map(|b| b.tag_ids.as_slice())
            .unwrap_or(&[]);
        let user = saved
            .tag_assignments
            .iter()
            .find(|t| t.resource_id == a.id)
            .map(|t| t.tag_ids.as_slice())
            .unwrap_or(&[]);
        let effective: HashSet<_> = base.iter().chain(user).collect();
        if effective.len() > MAX_ITEMS {
            return Err("单资源有效标签最多 100 个".into());
        }
        add_refs(&mut total, effective.len())?;
        for tags in builtin_groups
            .iter()
            .map(|g| &g.group.tag_ids)
            .chain(saved.groups.iter().map(|g| &g.tag_ids))
        {
            if tags.iter().any(|t| effective.contains(t)) {
                add_refs(&mut total, 1)?;
            }
        }
    }
    Ok(())
}
fn resource_refs(
    a: &Application,
    sites: &HashMap<&str, &Site>,
    ids: &mut HashSet<String>,
    total: &mut usize,
) -> Result<(), String> {
    if a.site_ids.len() > MAX_ITEMS || a.reference_rules.len() > MAX_ITEMS {
        return Err("每个资源最多引用 100 个端点、包含 100 条规则".into());
    }
    add_refs(total, a.site_ids.len())?;
    add_refs(total, a.reference_rules.len())?;
    let mut seen = HashSet::new();
    for id in &a.site_ids {
        let site = sites
            .get(id.as_str())
            .ok_or_else(|| format!("资源引用不存在的端点 {id}"))?;
        if !seen.insert(id) {
            return Err("资源不能重复引用端点".into());
        }
        if a.kind == ApplicationKind::Website {
            website_site(site)?;
        }
    }
    for r in &a.reference_rules {
        custom_id(&r.id, ids, "规则")?;
        validate_rule(r, false)?;
    }
    Ok(())
}

pub(super) fn snapshot(
    saved: &V3Catalog,
    sites: Vec<CatalogSite>,
    builtins: &[CatalogApplication],
    builtin_groups: &[CatalogGroup],
) -> Snapshot {
    let mut applications: Vec<_> = builtins
        .iter()
        .cloned()
        .chain(saved.applications.iter().cloned().map(|application| {
            let limitations = if application.category == ApplicationCategory::Web
                && application.kind == ApplicationKind::Application
            {
                vec!["需确认类型：旧 web 资料不全为明确 HTTP(S) 连接，原连接已完整保留".into()]
            } else {
                vec![]
            };
            CatalogApplication {
                application,
                tag_ids: vec![],
                supplement: None,
                builtin: false,
                sources: vec![],
                missing: vec![],
                limitations,
                summary: String::new(),
                status: "custom".into(),
                environment: None,
                version: None,
            }
        }))
        .collect();
    for a in &mut applications {
        if let Some(t) = saved
            .tag_assignments
            .iter()
            .find(|t| t.resource_id == a.application.id)
        {
            for id in &t.tag_ids {
                if !a.tag_ids.contains(id) {
                    a.tag_ids.push(id.clone());
                }
            }
        }
        if let Some(s) = saved
            .resource_supplements
            .iter()
            .find(|s| s.resource_id == a.application.id)
        {
            a.application.site_ids.extend(s.site_ids.clone());
            a.application
                .reference_rules
                .extend(s.reference_rules.clone());
            a.supplement = Some(s.clone());
        }
    }
    let groups = builtin_groups
        .iter()
        .cloned()
        .chain(saved.groups.iter().map(|g| CatalogGroup {
            group: Group {
                id: g.id.clone(),
                name: g.name.clone(),
                description: g.description.clone(),
                tag_ids: g.tag_ids.clone(),
                application_ids: vec![],
            },
            builtin: false,
        }))
        .map(|mut g| {
            g.group.application_ids = applications
                .iter()
                .filter(|a| g.group.tag_ids.iter().any(|t| a.tag_ids.contains(t)))
                .map(|a| a.application.id.clone())
                .collect();
            g
        })
        .collect();
    let tags = builtin_tags(builtin_groups)
        .into_iter()
        .chain(saved.tags.iter().cloned().map(|tag| CatalogTag {
            tag,
            builtin: false,
        }))
        .collect();
    Snapshot {
        version: VERSION,
        revision: saved.revision,
        sites,
        applications,
        groups,
        tags,
        tag_assignments: saved.tag_assignments.clone(),
        resource_supplements: saved.resource_supplements.clone(),
    }
}

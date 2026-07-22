use crate::types::PackageReq;
use libsemverator::range::Range as VersionReq;
use std::collections::HashMap;
use std::error::Error;

/// Projects whose distinct version lines are parallel-installable (different
/// sonames / ICU majors / abseil LTS namespaces). When constraints cannot
/// intersect we keep the extra lines as separate `PackageReq` entries instead
/// of failing the graph. Note: these extra lines are surfaced in the resolved
/// set but their own deps are not re-hydrated — fine while alt-line deps match
/// the primary line (openssl/abseil/unicode), revisit if that stops holding.
///
/// - unicode.org: ICU major ABI (see pantry#4104, pkgx#899)
/// - openssl.org: libssl.so.1.1 vs libssl.so.3
/// - abseil.io: LTS inline-namespace + soversion (20250127 vs 20250512, …)
const MULTI_VERSION_PROJECTS: &[&str] = &["unicode.org", "openssl.org", "abseil.io"];

fn is_multi_version(project: &str) -> bool {
    MULTI_VERSION_PROJECTS.contains(&project)
}

/// Record an extra constraint for a multi-version project, merging into an
/// existing additional entry when the ranges intersect.
fn push_additional(additional: &mut Vec<PackageReq>, pkg: PackageReq) {
    for existing in additional.iter_mut().filter(|p| p.project == pkg.project) {
        if let Ok(constraint) = intersect_constraints(&existing.constraint, &pkg.constraint) {
            existing.constraint = constraint;
            return;
        }
    }
    additional.push(pkg);
}

#[derive(Clone)]
struct Node {
    parent: Option<Box<Node>>,
    pkg: PackageReq,
}

impl Node {
    fn new(pkg: PackageReq, parent: Option<Box<Node>>) -> Self {
        Self { parent, pkg }
    }

    fn count(&self) -> usize {
        let mut count = 0;
        let mut node = self.parent.as_ref();
        while let Some(parent_node) = node {
            count += 1;
            node = parent_node.parent.as_ref();
        }
        count
    }
}

/// Hydrates dependencies and returns a topologically sorted list of packages.
pub async fn hydrate<F>(
    input: &Vec<PackageReq>,
    get_deps: F,
) -> Result<Vec<PackageReq>, Box<dyn Error>>
where
    F: Fn(String) -> Result<Vec<PackageReq>, Box<dyn Error>>,
{
    let dry = condense(input)?;
    let mut graph: HashMap<String, Box<Node>> = HashMap::new();
    let mut stack: Vec<Box<Node>> = vec![];
    let mut additional: Vec<PackageReq> = vec![];

    for pkg in dry.iter() {
        if let Some(node) = graph.get_mut(&pkg.project) {
            match intersect_constraints(&node.pkg.constraint, &pkg.constraint) {
                Ok(constraint) => {
                    node.pkg.constraint = constraint;
                    stack.push(node.clone());
                }
                Err(e) => {
                    if is_multi_version(&pkg.project) {
                        push_additional(&mut additional, pkg.clone());
                    } else {
                        return Err(format!("{} for {}", e, pkg.project).into());
                    }
                }
            }
        } else {
            let node = Box::new(Node::new(pkg.clone(), None));
            graph.insert(pkg.project.clone(), node.clone());
            stack.push(node);
        }
    }

    while let Some(current) = stack.pop() {
        for child_pkg in get_deps(current.pkg.project.clone())? {
            let was_new = !graph.contains_key(&child_pkg.project);
            let child_node = graph
                .entry(child_pkg.project.clone())
                .or_insert_with(|| Box::new(Node::new(child_pkg.clone(), Some(current.clone()))));

            if was_new {
                // Fresh node already carries child_pkg.constraint.
                stack.push(child_node.clone());
                continue;
            }

            // Already have a graph node: try the primary constraint, then any
            // additional lines for this multi-version project.
            let intersection =
                intersect_constraints(&child_node.pkg.constraint, &child_pkg.constraint);
            if let Ok(constraint) = intersection {
                child_node.pkg.constraint = constraint;
                stack.push(child_node.clone());
            } else if is_multi_version(&child_pkg.project) {
                push_additional(&mut additional, child_pkg);
            } else {
                return Err(
                    format!("{} for {}", intersection.unwrap_err(), child_pkg.project).into(),
                );
            }
        }
    }

    let mut pkgs: Vec<&Box<Node>> = graph.values().collect();
    pkgs.sort_by_key(|node| node.count());
    let mut pkgs: Vec<PackageReq> = pkgs.into_iter().map(|node| node.pkg.clone()).collect();

    pkgs.extend(additional);

    Ok(pkgs)
}

/// Condenses a list of `PackageReq` by intersecting constraints for duplicates.
/// Multi-version projects keep non-intersecting constraints as separate entries.
fn condense(pkgs: &Vec<PackageReq>) -> Result<Vec<PackageReq>, Box<dyn Error>> {
    let mut out: Vec<PackageReq> = vec![];
    for pkg in pkgs {
        if let Some(existing) = out.iter_mut().find(|p| p.project == pkg.project) {
            match intersect_constraints(&existing.constraint, &pkg.constraint) {
                Ok(constraint) => existing.constraint = constraint,
                Err(e) => {
                    if is_multi_version(&pkg.project) {
                        // merge into a later non-intersecting sibling if possible
                        let mut merged = false;
                        for sibling in out.iter_mut().filter(|p| p.project == pkg.project).skip(1) {
                            if let Ok(constraint) =
                                intersect_constraints(&sibling.constraint, &pkg.constraint)
                            {
                                sibling.constraint = constraint;
                                merged = true;
                                break;
                            }
                        }
                        if !merged {
                            out.push(pkg.clone());
                        }
                    } else {
                        return Err(format!("{} for {}", e, pkg.project).into());
                    }
                }
            }
        } else {
            out.push(pkg.clone());
        }
    }
    Ok(out)
}

/// Intersects two version constraints.
fn intersect_constraints(a: &VersionReq, b: &VersionReq) -> Result<VersionReq, Box<dyn Error>> {
    a.intersect(b).map_err(|e| e.into())
}

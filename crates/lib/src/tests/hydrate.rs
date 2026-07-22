use crate::hydrate::hydrate;
use crate::types::PackageReq;
use libsemverator::range::Range as VersionReq;

fn req(project: &str, constraint: &str) -> PackageReq {
    PackageReq {
        project: project.to_string(),
        constraint: VersionReq::parse(constraint).unwrap(),
    }
}

fn pkgs_for<'a>(pkgs: &'a [PackageReq], project: &str) -> Vec<&'a PackageReq> {
    pkgs.iter().filter(|p| p.project == project).collect()
}

/// True if some hydrated line for `project` intersects `range` (same ABI line).
fn has_line(pkgs: &[PackageReq], project: &str, range: &str) -> bool {
    let want = VersionReq::parse(range).unwrap();
    pkgs_for(pkgs, project)
        .into_iter()
        .any(|p| p.constraint.intersect(&want).is_ok())
}

/// Assert two ranges remain disjoint (cannot be collapsed).
fn assert_disjoint(a: &str, b: &str) {
    assert!(VersionReq::parse(a)
        .unwrap()
        .intersect(&VersionReq::parse(b).unwrap())
        .is_err());
}

#[tokio::test]
async fn hydrates_unicode_multi() {
    let input = vec![req("npmjs.com", "*"), req("python.org", "~3.9")];
    let pkgs = hydrate(&input, |project| match project.as_str() {
        "python.org" => Ok(vec![req("unicode.org", "^73")]),
        "npmjs.com" => Ok(vec![req("unicode.org", "^71")]),
        _ => Ok(vec![]),
    })
    .await
    .unwrap();

    assert_eq!(pkgs_for(&pkgs, "unicode.org").len(), 2);
    assert!(has_line(&pkgs, "unicode.org", "^71"));
    assert!(has_line(&pkgs, "unicode.org", "^73"));
    assert_disjoint("^71", "^73");
}

#[tokio::test]
async fn hydrates_openssl_multi() {
    // python locks ^1.1; cryptography needs ^3 — must coexist
    let input = vec![req("python.org", "*"), req("cryptography.io", "*")];
    let pkgs = hydrate(&input, |project| match project.as_str() {
        "python.org" => Ok(vec![req("openssl.org", "^1.1")]),
        "cryptography.io" => Ok(vec![req("openssl.org", "^3")]),
        _ => Ok(vec![]),
    })
    .await
    .unwrap();

    assert_eq!(pkgs_for(&pkgs, "openssl.org").len(), 2);
    assert!(has_line(&pkgs, "openssl.org", "^1.1"));
    assert!(has_line(&pkgs, "openssl.org", "^3"));
    assert_disjoint("^1.1", "^3");
}

#[tokio::test]
async fn hydrates_abseil_multi() {
    // re2 on one LTS line, grpc on another
    let input = vec![req("github.com/google/re2", "*"), req("grpc.io", "*")];
    let pkgs = hydrate(&input, |project| match project.as_str() {
        "github.com/google/re2" => Ok(vec![req("abseil.io", "^20250127")]),
        "grpc.io" => Ok(vec![req("abseil.io", ">=20250512")]),
        _ => Ok(vec![]),
    })
    .await
    .unwrap();

    assert_eq!(pkgs_for(&pkgs, "abseil.io").len(), 2);
    assert!(has_line(&pkgs, "abseil.io", "^20250127"));
    assert!(has_line(&pkgs, "abseil.io", ">=20250512"));
    assert_disjoint("^20250127", ">=20250512");
}

#[tokio::test]
async fn hydrates_openssl_dry_input() {
    // explicit +openssl^1.1 +openssl^3
    let input = vec![req("openssl.org", "^1.1"), req("openssl.org", "^3")];
    let pkgs = hydrate(&input, |_| Ok(vec![])).await.unwrap();

    assert_eq!(pkgs_for(&pkgs, "openssl.org").len(), 2);
    assert!(has_line(&pkgs, "openssl.org", "^1.1"));
    assert!(has_line(&pkgs, "openssl.org", "^3"));
}

#[tokio::test]
async fn hydrates_multi_version_three_way() {
    // three consumers, two openssl lines (two share ^1.1, one needs ^3)
    // order cryptography first so ^3 is the graph node and both ^1.1 merge
    // into a single additional entry.
    let input = vec![
        req("cryptography.io", "*"),
        req("python.org", "*"),
        req("curl.se", "*"),
    ];
    let pkgs = hydrate(&input, |project| match project.as_str() {
        "python.org" | "curl.se" => Ok(vec![req("openssl.org", "^1.1")]),
        "cryptography.io" => Ok(vec![req("openssl.org", "^3")]),
        _ => Ok(vec![]),
    })
    .await
    .unwrap();

    assert_eq!(pkgs_for(&pkgs, "openssl.org").len(), 2);
    assert!(has_line(&pkgs, "openssl.org", "^1.1"));
    assert!(has_line(&pkgs, "openssl.org", "^3"));
}

#[tokio::test]
async fn hydrates_cannot_intersect() {
    let input = vec![req("npmjs.com", "*"), req("python.org", "~3.9")];
    let err = hydrate(&input, |project| match project.as_str() {
        "python.org" => Ok(vec![req("nodejs.com", "^73")]),
        "npmjs.com" => Ok(vec![req("nodejs.com", "^71")]),
        _ => Ok(vec![]),
    })
    .await
    .unwrap_err();

    assert!(err.to_string().contains("nodejs.com"));
}

#[tokio::test]
async fn hydrates_compatible_intersect() {
    let input = vec![req("pipenv.pypa.io", "*"), req("python.org", "~3.9")];
    let pkgs = hydrate(&input, |project| match project.as_str() {
        "pipenv.pypa.io" => Ok(vec![req("python.org", ">=3.7")]),
        _ => Ok(vec![]),
    })
    .await
    .unwrap();

    let pythons = pkgs_for(&pkgs, "python.org");
    assert_eq!(pythons.len(), 1);
    // dry ~3.9 wins over looser >=3.7
    assert!(has_line(&pkgs, "python.org", "~3.9"));
    assert!(!has_line(&pkgs, "python.org", "~3.10"));
    assert!(!has_line(&pkgs, "python.org", "~3.8"));
}

#[tokio::test]
async fn hydrates_multi_version_dry_condense_compatible() {
    // two compatible dry openssl constraints still collapse to one
    let input = vec![req("openssl.org", "^1.1"), req("openssl.org", ">=1.1.1")];
    let pkgs = hydrate(&input, |_| Ok(vec![])).await.unwrap();

    assert_eq!(pkgs_for(&pkgs, "openssl.org").len(), 1);
    assert!(has_line(&pkgs, "openssl.org", "^1.1"));
    assert!(!has_line(&pkgs, "openssl.org", "^3"));
}

use super::*;
use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn verification_and_exact_entry_selection_precede_extraction() -> io::Result<()> {
    let windows = zip_bytes(&[
        ("../outside", b"do not extract"),
        ("config.example.yaml", b"do not extract"),
        ("cli-proxy-api.exe", b"binary bytes"),
    ]);
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    let mut header = tar::Header::new_gnu();
    header.set_mode(/*mode*/ 0o755);
    header.set_size(/*size*/ 12);
    header.set_cksum();
    builder.append_data(&mut header, "cli-proxy-api", &b"binary bytes"[..])?;
    let unix = builder.into_inner()?.finish()?;
    for (platform, archive) in [("windows_amd64", windows), ("linux_amd64", unix)] {
        let digest = format!("{:x}", Sha256::digest(&archive));
        let release = Release {
            platform,
            sha256: &digest,
        };
        assert_eq!(extract_binary(&archive, &release)?, b"binary bytes");
        let mut corrupted = archive.clone();
        corrupted[0] ^= 1;
        assert!(
            extract_binary(&corrupted, &release)
                .unwrap_err()
                .to_string()
                .contains("SHA256")
        );
    }
    let missing = zip_bytes(&[("other.exe", b"other")]);
    let digest = format!("{:x}", Sha256::digest(&missing));
    assert!(
        extract_binary(
            &missing,
            &Release {
                platform: "windows_amd64",
                sha256: &digest
            }
        )
        .is_err()
    );
    let invalid = b"not an archive";
    let digest = format!("{:x}", Sha256::digest(invalid));
    assert!(
        extract_binary(
            invalid,
            &Release {
                platform: "linux_amd64",
                sha256: &digest
            }
        )
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn failed_provision_cannot_promote_or_modify_other_files() -> io::Result<()> {
    let server = MockServer::start().await;
    let home = tempfile::tempdir()?;
    let independent = home.path().join("other-installation");
    fs::write(&independent, b"user installation")?;
    let managed = home.path().join("managed");
    codex_uds::prepare_private_socket_directory(&managed).await?;
    let factory = HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault);
    fs::write(
        managed.join("cli-proxy-api.exe.pending"),
        b"interrupted staging",
    )?;
    for (index, archive, expected_digest) in [
        (
            0,
            zip_bytes(&[("cli-proxy-api.exe", b"incompatible executable")]),
            None,
        ),
        (1, zip_bytes(&[("other.exe", b"missing executable")]), None),
        (2, b"corrupt archive".to_vec(), None),
        (
            3,
            zip_bytes(&[("cli-proxy-api.exe", b"binary")]),
            Some("wrong checksum"),
        ),
    ] {
        let digest = format!("{:x}", Sha256::digest(&archive));
        let release = Release {
            platform: "windows_amd64",
            sha256: expected_digest.unwrap_or(&digest),
        };
        let route = format!("/archive-{index}");
        Mock::given(method("GET"))
            .and(path(&route))
            .respond_with(ResponseTemplate::new(/*status_code*/ 200).set_body_bytes(archive))
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            provision(
                &managed,
                &release,
                &format!("{}{route}", server.uri()),
                &factory
            )
            .await
            .is_err()
        );
        assert_eq!(fs::read_dir(&managed)?.count(), 0);
        assert_eq!(fs::read(&independent)?, b"user installation");
    }
    Mock::given(method("GET"))
        .and(path("/oversize"))
        .respond_with(
            ResponseTemplate::new(/*status_code*/ 200).insert_header("content-length", "67108865"),
        )
        .mount(&server)
        .await;
    assert!(
        provision(
            &managed,
            &platform_release()?,
            &format!("{}/oversize", server.uri()),
            &factory
        )
        .await
        .is_err()
    );
    assert_eq!(fs::read_dir(&managed)?.count(), 0);
    Ok(())
}

#[tokio::test]
async fn explicit_invalid_override_does_not_fall_back_or_download() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    let factory = HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault);
    let missing = home.path().join("missing.exe");
    for explicit in [Path::new("relative"), missing.as_path()] {
        let error = resolve(home.path(), Some(explicit), Path::new(""), &factory)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("CODEX_CLI_PROXY_EXECUTABLE"));
        assert_eq!(fs::read_dir(home.path())?.count(), 0);
    }
    assert_eq!(installed_executable(/*explicit*/ None, [missing])?, None);
    Ok(())
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn installed_and_managed_candidates_avoid_download() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    #[cfg(unix)]
    let installed = {
        use std::os::unix::fs::PermissionsExt;
        let path = home.path().join("installed");
        fs::write(
            &path,
            b"#!/bin/sh\nprintf 'CLIProxyAPI Version: 7.3.14, test\\n'\n",
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(/*mode*/ 0o700))?;
        path
    };
    #[cfg(windows)]
    let installed = {
        let Some(path) = std::env::var_os("CODEX_TEST_CLI_PROXY_EXE") else {
            return Ok(());
        };
        PathBuf::from(path)
    };
    let expected = installed.canonicalize()?;
    assert_eq!(
        installed_executable(Some(&installed), [])?,
        Some(expected.clone())
    );
    assert_eq!(
        installed_executable(
            /*explicit*/ None,
            [home.path().join("missing"), installed.clone()]
        )?,
        Some(expected.clone())
    );
    let factory = HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault);
    assert_eq!(
        resolve(home.path(), /*explicit*/ None, &installed, &factory).await?,
        expected
    );
    assert!(
        !home
            .path()
            .join(format!("managed-{TESTED_VERSION}"))
            .exists()
    );
    let release = platform_release()?;
    let managed = home.path().join(format!("managed-{TESTED_VERSION}"));
    codex_uds::prepare_private_socket_directory(&managed).await?;
    let binary = managed.join(release.binary_name());
    fs::copy(&installed, &binary)?;
    assert_eq!(
        resolve(home.path(), /*explicit*/ None, Path::new(""), &factory).await?,
        binary.canonicalize()?
    );
    Ok(())
}

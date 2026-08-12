use image::ImageFormat;
use reqwest::multipart::{Form, Part};
use serde_json::Value;
use std::env;
use std::io::Cursor;

use std::time::Duration;

/// Bound on establishing a connection to Drive. Independent of payload size.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Total bound for the metadata calls (folder lookup, folder create, sharing).
///
/// These carry a small JSON request and response, so there is no long
/// legitimate case for a total bound to accommodate.
const METADATA_TIMEOUT: Duration = Duration::from_secs(10);

/// Total bound for the picture upload.
///
/// `reqwest::Client::timeout` covers body transfer, and this sends the whole
/// picture in one multipart body. `di_handlers.rs` caps that at 5 MiB, so at
/// 60s the slowest tolerated throughput is roughly 85 KB/s, far below any
/// usable connection. Copying `METADATA_TIMEOUT` here would abort real uploads
/// on slow links, which is the trap identified in file-management#54.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Base URL for the Drive API, overridable so the calls can be tested.
///
/// The endpoints were hardcoded to googleapis.com, which left this module with
/// no test seam at all and, not coincidentally, zero tests (#51). Same approach
/// as `brevo_api_base_url` in auth-service.
fn drive_api_base() -> String {
    std::env::var("GOOGLE_DRIVE_API_BASE_URL")
        .unwrap_or_else(|_| "https://www.googleapis.com".to_string())
}

/// A client for the small metadata calls.
fn metadata_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(METADATA_TIMEOUT)
        .build()
}

/// A client for the picture upload, with room for the body.
///
/// `connect_timeout` alone is not sufficient: a server that completes the
/// handshake and then never answers leaves an established, idle socket, which
/// the kernel does not treat as an error short of TCP keepalive. Verified by
/// mutation in file-management#54.
fn upload_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(UPLOAD_TIMEOUT)
        .build()
}


// Upload profile picture to Google Drive (matches Node.js implementation exactly)
pub async fn upload_profile_picture(
    user_id: &str,
    user_email: &str,
    file_data: Vec<u8>,
    _file_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // Process image: create optimized profile picture (400x400 max, 200x200 min)
    // Same logic as Node.js sharp processing
    let image = image::load_from_memory(&file_data)?;

    // Resize to 400x400 max, maintaining aspect ratio
    let resized = image.resize(400, 400, image::imageops::FilterType::Lanczos3);

    // Then crop to 200x200 square from center
    let cropped = resized.crop_imm(
        (resized.width().saturating_sub(200)) / 2,
        (resized.height().saturating_sub(200)) / 2,
        200.min(resized.width()),
        200.min(resized.height()),
    );

    // Convert to JPEG with 90% quality (same as Node.js)
    let mut jpeg_data = Vec::new();
    cropped.write_to(&mut Cursor::new(&mut jpeg_data), ImageFormat::Jpeg)?;

    // Get Google Drive access token (simplified - in production use proper OAuth2)
    let access_token = env::var("GOOGLE_DRIVE_ACCESS_TOKEN")
        .map_err(|_| "Google Drive access token not configured")?;

    // Create profile folder structure (matches Node.js createProfilePhotoFolder)
    let folder_id = create_profile_folder(user_id, user_email, &access_token).await?;

    // Upload file to Google Drive
    let file_id = upload_to_drive(
        &format!("profile_{}_{}.jpg", user_id, chrono::Utc::now().timestamp()),
        jpeg_data,
        &folder_id,
        &access_token,
    )
    .await?;

    // Make file publicly accessible (matches Node.js shareFile)
    make_file_public(&file_id, &access_token).await?;

    // Return Google Drive thumbnail URL (same format as Node.js)
    Ok(format!(
        "https://drive.google.com/thumbnail?id={}&sz=w200-h200",
        file_id
    ))
}

// Create profile folder structure in Google Drive
/// Fail on a non-success status, naming it.
///
/// Three calls in this module previously parsed the body straight to JSON and
/// inferred failure from a missing field. That reads a *rejection* as a
/// *shape*: it worked by accident where the expected field happened to be
/// absent from error bodies, and not at all where the code was looking for
/// something an error body might still lack (#55).
///
/// The search case was the worst of the three. An error body has no `files`
/// key, so a throttled search looked exactly like a search that found nothing,
/// and control fell through to creating a duplicate folder.
/// Returns `String` rather than the module's boxed error because this is held
/// across an await: `Box<dyn Error>` is not `Send`, and `upload_profile_picture`
/// is behind an `#[async_trait]` that requires the future to be.
async fn ensure_success(response: reqwest::Response, what: &str) -> Result<reqwest::Response, String> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    log::error!("Drive rejected {}: status={} body={}", what, status, body);
    Err(format!("Drive rejected {}: {}", what, status))
}

async fn create_profile_folder(
    user_id: &str,
    user_email: &str,
    access_token: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // Check if folder already exists first
    let search_query = format!(
        "name='profile_photos_{}' and mimeType='application/vnd.google-apps.folder'",
        user_id
    );

    let client = metadata_client()?;
    let api_base = drive_api_base();
    let response = client
        .get(format!("{}/drive/v3/files", api_base))
        .bearer_auth(access_token)
        .query(&[("q", &search_query)])
        .send()
        .await?;

    // Checked before parsing. Without this a 429 fell through to the create
    // branch below, because an error body has no `files` key and so looked
    // identical to an empty result (#55).
    // Split deliberately. Chaining `?` onto the `map_err` keeps a
    // `Box<dyn Error>` alive across the `.json().await` below, and that box is
    // not `Send`, which breaks the `#[async_trait]` bound on
    // `upload_profile_picture`. Returning immediately keeps it off the future.
    let response = match ensure_success(response, "the folder search").await {
        Ok(response) => response,
        Err(e) => return Err(e.into()),
    };
    let search_result: Value = response.json().await?;

    // If folder exists, return its ID
    if let Some(files) = search_result.get("files").and_then(|f| f.as_array()) {
        if !files.is_empty() {
            if let Some(id) = files[0].get("id").and_then(|i| i.as_str()) {
                return Ok(id.to_string());
            }
        }
    }

    // Create new folder
    let folder_metadata = serde_json::json!({
        "name": format!("profile_photos_{}", user_id),
        "mimeType": "application/vnd.google-apps.folder",
        "description": format!("Profile photos for user: {}", user_email)
    });

    let response = client
        .post(format!("{}/drive/v3/files", api_base))
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .json(&folder_metadata)
        .send()
        .await?;

    // Split deliberately. Chaining `?` onto the `map_err` keeps a
    // `Box<dyn Error>` alive across the `.json().await` below, and that box is
    // not `Send`, which breaks the `#[async_trait]` bound on
    // `upload_profile_picture`. Returning immediately keeps it off the future.
    let response = match ensure_success(response, "the folder creation").await {
        Ok(response) => response,
        Err(e) => return Err(e.into()),
    };
    let result: Value = response.json().await?;

    result
        .get("id")
        .and_then(|id| id.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Drive accepted the folder creation but returned no id".into())
}

// Upload file to Google Drive
async fn upload_to_drive(
    file_name: &str,
    file_data: Vec<u8>,
    parent_folder_id: &str,
    access_token: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let metadata = serde_json::json!({
        "name": file_name,
        "parents": [parent_folder_id]
    });

    let form = Form::new().text("metadata", metadata.to_string()).part(
        "data",
        Part::bytes(file_data)
            .file_name(file_name.to_string())
            .mime_str("image/jpeg")?,
    );

    let client = upload_client()?;
    let response = client
        .post(format!(
            "{}/upload/drive/v3/files?uploadType=multipart",
            drive_api_base()
        ))
        .bearer_auth(access_token)
        .multipart(form)
        .send()
        .await?;

    // Split deliberately. Chaining `?` onto the `map_err` keeps a
    // `Box<dyn Error>` alive across the `.json().await` below, and that box is
    // not `Send`, which breaks the `#[async_trait]` bound on
    // `upload_profile_picture`. Returning immediately keeps it off the future.
    let response = match ensure_success(response, "the file upload").await {
        Ok(response) => response,
        Err(e) => return Err(e.into()),
    };
    let result: Value = response.json().await?;

    result
        .get("id")
        .and_then(|id| id.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Drive accepted the upload but returned no id".into())
}

// Make file publicly accessible
async fn make_file_public(
    file_id: &str,
    access_token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let permission = serde_json::json!({
        "role": "reader",
        "type": "anyone"
    });

    let client = metadata_client()?;
    let response = client
        .post(format!(
            "{}/drive/v3/files/{}/permissions",
            drive_api_base(),
            file_id
        ))
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .json(&permission)
        .send()
        .await?;

    // `?` above covers transport failures only. A 403 or 404 from Drive is a
    // perfectly successful HTTP exchange, so without this the response was
    // discarded and the function reported `Ok(())` while the file stayed
    // private. This is the last step of the avatar upload chain, so the caller
    // was told the upload worked and the stored URL then 404d for every
    // viewer, with nothing in the logs because nothing looked (#53).
    //
    // The other calls in this module catch failures only incidentally, by
    // parsing the body and requiring an `id` field that an error response does
    // not carry. A sharing request has no such field to lean on.
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        log::error!(
            "Drive refused to make file {} public: status={} body={}",
            file_id,
            status,
            body
        );
        return Err(format!("Drive rejected the sharing request: {}", status).into());
    }

    Ok(())
}

#[cfg(test)]
mod bounded_drive_calls {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // `GOOGLE_DRIVE_API_BASE_URL` is process-global, so these serialize
    // against each other the way auth-service's Brevo tests do.
    lazy_static::lazy_static! {
        static ref DRIVE_ENV_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
    }

    /// A stalled Drive must not hold the avatar request open.
    ///
    /// Stalls the first leg (the folder lookup), which is what an upload hits
    /// first, so this pins the property that actually matters to a caller:
    /// `POST /api/users/profile-picture` cannot hang indefinitely.
    ///
    /// A server that completes the handshake and then never answers leaves an
    /// established, idle socket. The kernel does not treat that as an error
    /// short of TCP keepalive, two hours away on Linux by default (#51).
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_stalled_drive_gives_up_instead_of_holding_the_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files"))
            .respond_with(
                // Stands in for "accepts the connection, never answers".
                ResponseTemplate::new(200).set_delay(Duration::from_secs(60)),
            )
            .mount(&server)
            .await;

        let _guard = DRIVE_ENV_MUTEX.lock().await;
        std::env::set_var("GOOGLE_DRIVE_API_BASE_URL", server.uri());

        let started = std::time::Instant::now();
        let result = create_profile_folder("token", "u-1", "a@b.c").await;
        let elapsed = started.elapsed();

        std::env::remove_var("GOOGLE_DRIVE_API_BASE_URL");

        assert!(result.is_err(), "a stalled Drive cannot have succeeded");

        // The bound is the test. The call fails either way once the endpoint
        // finally answers; without a timeout it takes the full delay.
        assert!(
            elapsed < Duration::from_secs(30),
            "took {elapsed:?}; the Drive metadata call is not bounded"
        );
    }

    /// A throttled search must not be read as "no folder exists".
    ///
    /// The search response is parsed straight to JSON and probed for a `files`
    /// array. An error body (`{"error": {...}}`) has no such key, so the
    /// `if let` did not match and control fell through to the create branch:
    /// a failed search was indistinguishable from an empty one.
    ///
    /// Drive rate-limits, so 429 is the likely case. Each throttled search
    /// created another `profile_photos_{user_id}` folder, since Drive permits
    /// duplicate names, scattering the user's avatars (#55).
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_throttled_search_does_not_create_a_duplicate_folder() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "error": { "code": 429, "message": "Rate Limit Exceeded" }
            })))
            .mount(&server)
            .await;
        // Deliberately mounted: if the fix works this is never called, and
        // that is the whole point. Without it a fall-through would silently
        // succeed here and the test would pass on the broken code.
        Mock::given(method("POST"))
            .and(path("/drive/v3/files"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "id": "duplicate-folder" })),
            )
            .mount(&server)
            .await;

        let _guard = DRIVE_ENV_MUTEX.lock().await;
        std::env::set_var("GOOGLE_DRIVE_API_BASE_URL", server.uri());

        let result = create_profile_folder("u-1", "a@b.c", "token").await;

        std::env::remove_var("GOOGLE_DRIVE_API_BASE_URL");

        assert!(
            result.is_err(),
            "a throttled search must fail, not fall through to creating a \
             second folder; got {result:?}"
        );
    }

    /// An empty result set is a real answer and must still create a folder.
    ///
    /// The fix must distinguish "the search worked and found nothing" from
    /// "the search did not work", not conflate them in the other direction.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn an_empty_search_still_creates_the_folder() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "files": [] })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/drive/v3/files"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "id": "new-folder" })),
            )
            .mount(&server)
            .await;

        let _guard = DRIVE_ENV_MUTEX.lock().await;
        std::env::set_var("GOOGLE_DRIVE_API_BASE_URL", server.uri());

        let result = create_profile_folder("u-2", "a@b.c", "token").await;

        std::env::remove_var("GOOGLE_DRIVE_API_BASE_URL");

        assert_eq!(result.expect("an empty search is a valid answer"), "new-folder");
    }

    /// And an existing folder is still reused rather than duplicated.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn an_existing_folder_is_reused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "files": [{ "id": "existing-folder" }] }),
            ))
            .mount(&server)
            .await;

        let _guard = DRIVE_ENV_MUTEX.lock().await;
        std::env::set_var("GOOGLE_DRIVE_API_BASE_URL", server.uri());

        let result = create_profile_folder("u-3", "a@b.c", "token").await;

        std::env::remove_var("GOOGLE_DRIVE_API_BASE_URL");

        assert_eq!(result.expect("an existing folder must be reused"), "existing-folder");
    }

    /// A rejected sharing request must not report success.
    ///
    /// `make_file_public` discarded its response. `?` covers transport
    /// failures only, so a 403 or 404 from Drive is a perfectly successful
    /// HTTP exchange and the function returned `Ok(())`.
    ///
    /// It is the last step of the avatar upload chain, so the caller was told
    /// the upload succeeded while the file stayed private. The stored
    /// `profilePictureUrl` then 404s for every viewer, and nothing appears in
    /// the logs because nothing looked (#53).
    ///
    /// The other three Drive calls in this module detect failure only
    /// incidentally: they parse the response and `.ok_or_else` on a missing
    /// `id` field, which an error body happens not to have. A sharing request
    /// has no such field, so this one had nothing at all.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_rejected_sharing_request_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/drive/v3/files/file-1/permissions"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": { "code": 403, "message": "Insufficient permissions" }
            })))
            .mount(&server)
            .await;

        let _guard = DRIVE_ENV_MUTEX.lock().await;
        std::env::set_var("GOOGLE_DRIVE_API_BASE_URL", server.uri());

        let result = make_file_public("file-1", "token").await;

        std::env::remove_var("GOOGLE_DRIVE_API_BASE_URL");

        assert!(
            result.is_err(),
            "a 403 from Drive means the file is not public; reporting Ok hides it"
        );
    }

    /// And the success path must still succeed.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn an_accepted_sharing_request_is_ok() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/drive/v3/files/file-2/permissions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "anyoneWithLink", "role": "reader", "type": "anyone"
            })))
            .mount(&server)
            .await;

        let _guard = DRIVE_ENV_MUTEX.lock().await;
        std::env::set_var("GOOGLE_DRIVE_API_BASE_URL", server.uri());

        let result = make_file_public("file-2", "token").await;

        std::env::remove_var("GOOGLE_DRIVE_API_BASE_URL");

        assert!(result.is_ok(), "a 200 must still be treated as success");
    }

    /// The upload bound must not be so tight that it breaks a real avatar.
    ///
    /// `di_handlers.rs` caps uploads at 5 MiB and `reqwest::Client::timeout`
    /// covers body transfer, so a value chosen only to stop hangs would abort
    /// legitimate uploads. This sends a full-size picture through
    /// `upload_client()` and pins that it still completes.
    ///
    /// The upload leg's own stall behaviour is not asserted here: its bound is
    /// 60s, so stalling it would mean a minute-long test. The metadata test
    /// above covers the request-path-cannot-hang property, and this covers the
    /// opposite direction, that the bound leaves room for the real payload.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_full_size_avatar_still_uploads_under_the_bound() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/upload/drive/v3/files"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "id": "uploaded-avatar" })),
            )
            .mount(&server)
            .await;

        let _guard = DRIVE_ENV_MUTEX.lock().await;
        std::env::set_var("GOOGLE_DRIVE_API_BASE_URL", server.uri());

        // The maximum the handler accepts.
        let picture = vec![0u8; 5 * 1024 * 1024];
        let result = upload_to_drive("token", picture, "avatar.jpg", "folder-1").await;

        std::env::remove_var("GOOGLE_DRIVE_API_BASE_URL");

        assert_eq!(
            result.expect("the bound must not break a full-size avatar upload"),
            "uploaded-avatar"
        );
    }
}

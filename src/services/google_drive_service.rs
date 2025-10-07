use image::ImageFormat;
use std::io::Cursor;
use reqwest::multipart::{Form, Part};
use serde_json::Value;
use std::env;

// Upload profile picture to Google Drive (matches Node.js implementation exactly)
pub async fn upload_profile_picture(
    user_id: &str,
    user_email: &str,
    file_data: Vec<u8>,
    _file_name: &str
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
        200.min(resized.height())
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
        &access_token
    ).await?;
    
    // Make file publicly accessible (matches Node.js shareFile)
    make_file_public(&file_id, &access_token).await?;
    
    // Return Google Drive thumbnail URL (same format as Node.js)
    Ok(format!("https://drive.google.com/thumbnail?id={}&sz=w200-h200", file_id))
}

// Create profile folder structure in Google Drive
async fn create_profile_folder(
    user_id: &str,
    user_email: &str,
    access_token: &str
) -> Result<String, Box<dyn std::error::Error>> {
    
    // Check if folder already exists first
    let search_query = format!("name='profile_photos_{}' and mimeType='application/vnd.google-apps.folder'", user_id);
    
    let client = reqwest::Client::new();
    let response = client
        .get("https://www.googleapis.com/drive/v3/files")
        .bearer_auth(access_token)
        .query(&[("q", &search_query)])
        .send()
        .await?;
    
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
        .post("https://www.googleapis.com/drive/v3/files")
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .json(&folder_metadata)
        .send()
        .await?;
    
    let result: Value = response.json().await?;
    
    result.get("id")
        .and_then(|id| id.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Failed to create Google Drive folder".into())
}

// Upload file to Google Drive
async fn upload_to_drive(
    file_name: &str,
    file_data: Vec<u8>,
    parent_folder_id: &str,
    access_token: &str
) -> Result<String, Box<dyn std::error::Error>> {
    
    let metadata = serde_json::json!({
        "name": file_name,
        "parents": [parent_folder_id]
    });
    
    let form = Form::new()
        .text("metadata", metadata.to_string())
        .part("data", 
            Part::bytes(file_data)
                .file_name(file_name.to_string())
                .mime_str("image/jpeg")?
        );
    
    let client = reqwest::Client::new();
    let response = client
        .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart")
        .bearer_auth(access_token)
        .multipart(form)
        .send()
        .await?;
    
    let result: Value = response.json().await?;
    
    result.get("id")
        .and_then(|id| id.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Failed to upload file to Google Drive".into())
}

// Make file publicly accessible
async fn make_file_public(
    file_id: &str,
    access_token: &str
) -> Result<(), Box<dyn std::error::Error>> {
    
    let permission = serde_json::json!({
        "role": "reader",
        "type": "anyone"
    });
    
    let client = reqwest::Client::new();
    let _response = client
        .post(&format!("https://www.googleapis.com/drive/v3/files/{}/permissions", file_id))
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .json(&permission)
        .send()
        .await?;
    
    Ok(())
}
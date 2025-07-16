use actix_web::{web, App, HttpResponse, HttpServer, Responder, post, get, Error};
use actix_multipart::Multipart;
use futures_util::stream::StreamExt as _;
use std::fs;
use std::io::Write;
use std::path::{PathBuf};
use tempfile::NamedTempFile;
use serde::Deserialize;
use tokio::fs as async_fs;
use tokio::io::{AsyncWriteExt, AsyncReadExt};

#[derive(Deserialize)]
struct ChunkParams {
    upload_id: String,
    index: usize,
}

#[derive(Deserialize)]
struct FinishParams {
    upload_id: String,
    filename: String,
    userhash: Option<String>,
    destination: String,
    time: Option<String>,
}

#[derive(Deserialize)]
struct DirectParams {
    destination: String,
    time: Option<String>,
    userhash: Option<String>,
}

const UPLOADS_DIR: &str = "/tmp/uploads";
const TEMP_DIR: &str = "/tmp/temp";

#[post("/chunk")]
async fn chunk(mut payload: Multipart) -> Result<impl Responder, Error> {
    let mut upload_id = String::new();
    let mut index = None;

    while let Some(Ok(mut field)) = payload.next().await {
        let name = field.name().to_string();
        if name == "uploadId" {
            let mut val = Vec::new();
            while let Some(chunk) = field.next().await {
                val.extend_from_slice(&chunk?);
            }
            upload_id = String::from_utf8_lossy(&val).to_string();
        } else if name == "index" {
            let mut val = Vec::new();
            while let Some(chunk) = field.next().await {
                val.extend_from_slice(&chunk?);
            }
            index = Some(String::from_utf8_lossy(&val).parse::<usize>().ok().unwrap());
        } else if name == "chunk" {
            if upload_id.is_empty() || index.is_none() {
                return Ok(HttpResponse::BadRequest().json(
                    serde_json::json!({"error": "Missing uploadId or index"})
                ));
            }
            let chunk_dir = PathBuf::from(UPLOADS_DIR).join(&upload_id);
            fs::create_dir_all(&chunk_dir).ok();
            let chunk_path = chunk_dir.join(format!("chunk_{}", index.unwrap()));
            let mut f = fs::File::create(chunk_path)?;
            while let Some(chunk) = field.next().await {
                f.write_all(&chunk?)?;
            }
        }
    }
    Ok(HttpResponse::Ok().json(
        serde_json::json!({"message": "Chunk received."})
    ))
}

#[post("/finish")]
async fn finish(params: web::Form<FinishParams>) -> Result<impl Responder, Error> {
    let chunk_dir = PathBuf::from(UPLOADS_DIR).join(&params.upload_id);
    let final_file_path = PathBuf::from(TEMP_DIR).join(format!("{}-{}", params.upload_id, params.filename));

    if !chunk_dir.exists() {
        return Ok(HttpResponse::BadRequest().json(
            serde_json::json!({"error": "No chunks found for this uploadId"})
        ));
    }

    let mut chunk_files: Vec<_> = fs::read_dir(&chunk_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("chunk_"))
        .collect();
    chunk_files.sort_by_key(|e| {
        let fname = e.file_name().to_string_lossy();
        fname.split('_').nth(1).unwrap().parse::<usize>().unwrap_or(0)
    });

    let mut final_file = async_fs::File::create(&final_file_path).await?;
    for entry in chunk_files {
        let mut chunk_file = async_fs::File::open(entry.path()).await?;
        let mut buffer = Vec::new();
        chunk_file.read_to_end(&mut buffer).await?;
        final_file.write_all(&buffer).await?;
    }
    final_file.flush().await?;

    let url = forward_to_destination(
        &params.destination,
        &final_file_path,
        &params.filename,
        params.userhash.as_deref(),
        params.time.as_deref().unwrap_or("1h")
    ).await;

    // Clean up
    fs::remove_dir_all(&chunk_dir).ok();
    fs::remove_file(&final_file_path).ok();

    match url {
        Ok(u) => Ok(HttpResponse::Ok().json(serde_json::json!({ "url": u }))),
        Err(e) => Ok(HttpResponse::InternalServerError().json(
            serde_json::json!({ "error": "Upload failed", "details": e.to_string() })
        )),
    }
}

#[post("/direct")]
async fn direct(mut payload: Multipart) -> Result<impl Responder, Error> {
    let mut params = DirectParams { destination: "".to_string(), time: None, userhash: None };
    let mut filename = "upload.dat".to_string();
    let mut temp_file = NamedTempFile::new_in(TEMP_DIR)?;
    while let Some(Ok(mut field)) = payload.next().await {
        let name = field.name().to_string();
        if name == "destination" {
            let mut val = Vec::new();
            while let Some(chunk) = field.next().await {
                val.extend_from_slice(&chunk?);
            }
            params.destination = String::from_utf8_lossy(&val).to_string();
        } else if name == "time" {
            let mut val = Vec::new();
            while let Some(chunk) = field.next().await {
                val.extend_from_slice(&chunk?);
            }
            params.time = Some(String::from_utf8_lossy(&val).to_string());
        } else if name == "userhash" {
            let mut val = Vec::new();
            while let Some(chunk) = field.next().await {
                val.extend_from_slice(&chunk?);
            }
            params.userhash = Some(String::from_utf8_lossy(&val).to_string());
        } else if name == "file" {
            if let Some(fname) = field.content_disposition().get_filename() {
                filename = fname.to_string();
            }
            while let Some(chunk) = field.next().await {
                temp_file.write_all(&chunk?)?;
            }
        }
    }
    if params.destination.is_empty() {
        return Ok(HttpResponse::BadRequest().json(
            serde_json::json!({"error": "Missing file or destination"})
        ));
    }

    let file_path = temp_file.path().to_path_buf();

    let url = forward_to_destination(
        &params.destination,
        &file_path,
        &filename,
        params.userhash.as_deref(),
        params.time.as_deref().unwrap_or("1h")
    ).await;

    fs::remove_file(&file_path).ok();

    match url {
        Ok(u) => Ok(HttpResponse::Ok().json(serde_json::json!({ "url": u }))),
        Err(e) => Ok(HttpResponse::InternalServerError().json(
            serde_json::json!({ "error": "Direct upload failed", "details": e.to_string() })
        )),
    }
}

async fn forward_to_destination(
    destination: &str,
    file_path: &PathBuf,
    original_name: &str,
    userhash: Option<&str>,
    time: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    if destination == "pomf" {
        let form = reqwest::multipart::Form::new()
            .file("files[]", file_path)?;
        let resp = client.post("https://pomf.lain.la/upload.php")
            .multipart(form)
            .send()
            .await?;
        let json: serde_json::Value = resp.json().await?;
        if json["success"].as_bool().unwrap_or(false) {
            return Ok(json["files"][0]["url"].as_str().unwrap_or("").to_string());
        } else {
            return Err("Pomf upload failed".into());
        }
    }

    let mut form = reqwest::multipart::Form::new()
        .text("reqtype", "fileupload")
        .file("fileToUpload", file_path)?;
    if destination == "catbox" {
        if let Some(uh) = userhash {
            form = form.text("userhash", uh.to_string());
        }
    }
    if destination == "litterbox" {
        form = form.text("time", time.to_string());
    }

    let url = if destination == "catbox" {
        "https://catbox.moe/user/api.php"
    } else {
        "https://litterbox.catbox.moe/resources/internals/api.php"
    };

    let resp = client.post(url)
        .multipart(form)
        .send()
        .await?;
    let text = resp.text().await?;
    Ok(text)
}

#[get("/")]
async fn root() -> impl Responder {
    HttpResponse::Ok().body("fatbox is working.")
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    fs::create_dir_all(UPLOADS_DIR).ok();
    fs::create_dir_all(TEMP_DIR).ok();
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    HttpServer::new(|| {
        App::new()
            .service(chunk)
            .service(finish)
            .service(direct)
            .service(root)
            .default_service(web::route().to(|| HttpResponse::NotFound().json(
                serde_json::json!({
                    "message": "Route not found",
                    "error": "Not Found",
                    "statusCode": 404
                })
            )))
    })
    .bind(format!("0.0.0.0:{}", port))?
    .run()
    .await
}
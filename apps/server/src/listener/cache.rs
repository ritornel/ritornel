//! Link-session downloads. Only decoded digests enter the cache lookup.

use super::*;
use ritornel_contract::transcode::ArtifactLayer;
use ritornel_engine::transcode::Codec;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub(super) async fn cached(
    State(core): State<Arc<Core>>,
    Extension(grant): Extension<SessionGrant>,
    Extension(peer): Extension<Option<Peer>>,
    Extension(layer): Extension<ArtifactLayer>,
    Path(key): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if peer.is_none() || !grant.permissions.contains(Permission::LibraryRead) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(key) = hex::decode_array::<32>(&key) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(service) = core.transcodes() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let transfer = match service.pool.transfer(key.into(), layer).await {
        Ok(Some(transfer)) => transfer,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            warn!(%error,"cache lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let entry = transfer.entry();
    let size = entry.size_bytes;
    let mime = match entry.codec() {
        Codec::Mp3 => "audio/mpeg",
        Codec::Opus => "audio/ogg",
        Codec::Flac => "audio/flac",
    };
    let etag = format!("\"{}\"", entry.file_hash);
    let mut response = Response::builder()
        .header(header::ETAG, &etag)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes");
    let (start, len) = if let Some(raw) = headers.get(header::RANGE)
        && headers
            .get(header::IF_RANGE)
            .is_none_or(|validator| validator.as_bytes() == etag.as_bytes())
    {
        let range = raw.to_str().ok().and_then(|value| byte_range(value, size));
        let Some((start, end)) =
            range.filter(|_| headers.get_all(header::RANGE).iter().count() == 1)
        else {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{size}"))],
            )
                .into_response();
        };
        response = response
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
        (start, end - start + 1)
    } else {
        (0, size)
    };
    let body = if method == Method::HEAD {
        axum::body::Body::empty()
    } else {
        let mut file = match tokio::fs::File::open(entry.path()).await {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return StatusCode::NOT_FOUND.into_response();
            }
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let chunks = futures_util::stream::try_unfold(
            (file, len, transfer),
            |(mut file, left, pin)| async move {
                if left == 0 {
                    return Ok(None);
                }
                let mut chunk = vec![0; left.min(AUDIO_CHUNK as u64) as usize];
                let n = file.read(&mut chunk).await?;
                if n == 0 {
                    return Err(std::io::Error::from(ErrorKind::UnexpectedEof));
                }
                chunk.truncate(n);
                Ok(Some((
                    axum::body::Bytes::from(chunk),
                    (file, left - n as u64, pin),
                )))
            },
        );
        axum::body::Body::from_stream(chunks)
    };
    response
        .header(header::CONTENT_LENGTH, len)
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn byte_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let (first, last) = value.strip_prefix("bytes=")?.split_once('-')?;
    let number = |s: &str| {
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    let end = size.checked_sub(1)?;
    if first.is_empty() {
        let suffix = number(last)?;
        return (suffix > 0).then(|| (size.saturating_sub(suffix), end));
    }
    let start = number(first)?;
    let last = if last.is_empty() {
        end
    } else {
        number(last)?.min(end)
    };
    (start <= last).then_some((start, last))
}

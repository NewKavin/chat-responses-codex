use super::ProtocolError;
use serde_json::{json, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImageSource {
    HttpsUrl(String),
    DataUrl(String),
    NativeFileId(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImagePart {
    pub source: ImageSource,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageDialect {
    pub https_url: bool,
    pub data_url: bool,
    pub detail: bool,
    pub native_file_id: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageAdaptation {
    pub value: Value,
    pub downgrade: Option<String>,
}

impl ImageDialect {
    pub fn all() -> Self {
        Self {
            https_url: true,
            data_url: true,
            detail: true,
            native_file_id: false,
        }
    }

    pub fn from_resolved(resolved: &crate::capabilities::ResolvedCapabilities) -> Self {
        Self {
            https_url: resolved.supports(crate::capabilities::Capability::ImageHttps),
            data_url: resolved.supports(crate::capabilities::Capability::ImageDataUrl),
            detail: resolved.supports(crate::capabilities::Capability::ImageDetail),
            native_file_id: resolved.supports(crate::capabilities::Capability::NativeFileId),
        }
    }
}

pub fn parse_data_url(value: &str) -> Result<(&str, &str), ProtocolError> {
    let rest = value
        .strip_prefix("data:")
        .ok_or(ProtocolError::UnsupportedImageSource)?;
    let (media, data) = rest
        .split_once(";base64,")
        .ok_or(ProtocolError::UnsupportedImageSource)?;
    if !media.starts_with("image/") || data.is_empty() {
        return Err(ProtocolError::UnsupportedImageSource);
    }
    Ok((media, data))
}

pub fn messages_image_to_chat_part(
    block: &Value,
    dialect: ImageDialect,
) -> Result<ImageAdaptation, ProtocolError> {
    let source = block
        .get("source")
        .and_then(Value::as_object)
        .ok_or(ProtocolError::MissingField("source"))?;
    let image = match source.get("type").and_then(Value::as_str) {
        Some("url") => ImagePart {
            source: classify_url(
                source
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or(ProtocolError::MissingField("url"))?,
            )?,
            detail: None,
        },
        Some("base64") => {
            let media = source
                .get("media_type")
                .and_then(Value::as_str)
                .ok_or(ProtocolError::MissingField("media_type"))?;
            let data = source
                .get("data")
                .and_then(Value::as_str)
                .ok_or(ProtocolError::MissingField("data"))?;
            let value = format!("data:{media};base64,{data}");
            parse_data_url(&value)?;
            ImagePart {
                source: ImageSource::DataUrl(value),
                detail: None,
            }
        }
        _ => return Err(ProtocolError::UnsupportedImageSource),
    };
    emit_chat_image(&image, dialect)
}

pub fn chat_image_to_responses_part(
    part: &Value,
    dialect: ImageDialect,
) -> Result<ImageAdaptation, ProtocolError> {
    let image_url = part
        .get("image_url")
        .ok_or(ProtocolError::MissingField("image_url"))?;
    let (url, detail) = match image_url {
        Value::String(url) => (url.as_str(), None),
        Value::Object(object) => (
            object
                .get("url")
                .and_then(Value::as_str)
                .ok_or(ProtocolError::MissingField("url"))?,
            object
                .get("detail")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
        _ => return Err(ProtocolError::UnsupportedImageSource),
    };
    emit_responses_image(
        &ImagePart {
            source: classify_url(url)?,
            detail,
        },
        dialect,
    )
}

pub fn responses_image_to_chat_part(
    part: &Value,
    dialect: ImageDialect,
) -> Result<ImageAdaptation, ProtocolError> {
    let url = part
        .get("image_url")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("image_url"))?;
    let detail = part
        .get("detail")
        .and_then(Value::as_str)
        .map(str::to_owned);
    emit_chat_image(
        &ImagePart {
            source: classify_url(url)?,
            detail,
        },
        dialect,
    )
}

fn classify_url(value: &str) -> Result<ImageSource, ProtocolError> {
    if value.starts_with("https://") {
        return Ok(ImageSource::HttpsUrl(value.to_owned()));
    }
    if value.starts_with("data:") {
        parse_data_url(value)?;
        return Ok(ImageSource::DataUrl(value.to_owned()));
    }
    Err(ProtocolError::UnsupportedImageSource)
}

fn emit_chat_image(
    image: &ImagePart,
    dialect: ImageDialect,
) -> Result<ImageAdaptation, ProtocolError> {
    let url = match &image.source {
        ImageSource::HttpsUrl(value) if dialect.https_url => value,
        ImageSource::DataUrl(value) if dialect.data_url => value,
        _ => return Err(ProtocolError::UnsupportedImageSource),
    };
    let mut nested = json!({"url": url});
    let downgrade = if let Some(detail) = image.detail.as_deref() {
        if dialect.detail {
            nested
                .as_object_mut()
                .unwrap()
                .insert("detail".into(), detail.into());
            None
        } else {
            Some("optional_image_detail".into())
        }
    } else {
        None
    };
    Ok(ImageAdaptation {
        value: json!({"type":"image_url","image_url":nested}),
        downgrade,
    })
}

fn emit_responses_image(
    image: &ImagePart,
    dialect: ImageDialect,
) -> Result<ImageAdaptation, ProtocolError> {
    let url = match &image.source {
        ImageSource::HttpsUrl(value) if dialect.https_url => value,
        ImageSource::DataUrl(value) if dialect.data_url => value,
        _ => return Err(ProtocolError::UnsupportedImageSource),
    };
    let mut value = json!({"type":"input_image","image_url":url});
    let downgrade = if let Some(detail) = image.detail.as_deref() {
        if dialect.detail {
            value
                .as_object_mut()
                .unwrap()
                .insert("detail".into(), detail.into());
            None
        } else {
            Some("optional_image_detail".into())
        }
    } else {
        None
    };
    Ok(ImageAdaptation { value, downgrade })
}

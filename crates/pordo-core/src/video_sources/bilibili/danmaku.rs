//! Bounded, segmented Bilibili VOD comments. No scripting or interactive payloads.
use super::*;
use prost::Message;

pub const SEGMENT_SECONDS: f64 = 360.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub id: i64,
    pub time: f64,
    pub mode: u8,
    pub size: u16,
    pub color: u32,
    pub text: String,
}

// Only fields used by the player are decoded; prost skips protocol extensions.
#[cfg(test)]
#[derive(Clone, PartialEq, Message)]
struct Segment {
    #[prost(message, repeated, tag = "1")]
    elems: Vec<Element>,
    #[prost(int32, tag = "2")]
    state: i32,
}
#[derive(Clone, PartialEq, Message)]
struct Element {
    #[prost(int64, tag = "1")]
    id: i64,
    #[prost(int32, tag = "2")]
    progress: i32,
    #[prost(int32, tag = "3")]
    mode: i32,
    #[prost(int32, tag = "4")]
    size: i32,
    #[prost(uint32, tag = "5")]
    color: u32,
    #[prost(string, tag = "7")]
    text: String,
}

pub fn parse(bytes: &[u8], index: u32) -> Result<Vec<Comment>> {
    ensure!(
        bytes.len() <= LIMIT && index > 0,
        "弹幕响应过大或分段无效 / Invalid danmaku segment"
    );
    if bytes.first() == Some(&b'{') {
        let value: Value = serde_json::from_slice(bytes)?;
        checked(&value)?;
        bail!("弹幕暂时不可用 / Danmaku unavailable");
    }
    // Decode the outer envelope incrementally, checking the count before allocation.
    let mut input = bytes;
    let mut elems = Vec::new();
    let mut state = 0i32;
    while !input.is_empty() {
        use prost::encoding::{DecodeContext, WireType, decode_key, decode_varint, skip_field};
        let (tag, wire) = decode_key(&mut input)?;
        match tag {
            1 => {
                ensure!(
                    wire == WireType::LengthDelimited && elems.len() < 12000,
                    "弹幕数量或格式无效 / Invalid danmaku payload"
                );
                let length = usize::try_from(decode_varint(&mut input)?)?;
                ensure!(
                    length <= input.len(),
                    "弹幕内容不完整 / Truncated danmaku response"
                );
                elems.push(Element::decode(&input[..length])?);
                input = &input[length..];
            }
            2 => prost::encoding::int32::merge(
                wire,
                &mut state,
                &mut input,
                DecodeContext::default(),
            )?,
            _ => skip_field(wire, tag, &mut input, DecodeContext::default())?,
        }
    }
    ensure!(
        state != 1,
        "视频已关闭弹幕 / Danmaku disabled for this video"
    );
    let start = (index - 1) as f64 * SEGMENT_SECONDS;
    let mut comments: Vec<_> = elems
        .into_iter()
        .filter_map(|e| {
            let time = e.progress as f64 / 1000.0;
            if !(1..=6).contains(&e.mode)
                || e.progress < 0
                || time < start
                || time > start + SEGMENT_SECONDS
                || e.text.len() > 2048
            {
                return None;
            }
            let text: String = e
                .text
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect();
            if text.trim().is_empty() {
                return None;
            }
            Some(Comment {
                id: e.id,
                time,
                mode: e.mode as u8,
                size: e.size.clamp(12, 50) as u16,
                color: e.color & 0xffffff,
                text,
            })
        })
        .collect();
    comments.sort_by(|a, b| a.time.total_cmp(&b.time).then(a.id.cmp(&b.id)));
    comments.dedup_by(|a, b| a.id != 0 && a.id == b.id && a.time == b.time);
    Ok(comments)
}

impl Bilibili {
    pub async fn danmaku_segment(&self, cid: u64, index: u32) -> Result<Vec<Comment>> {
        ensure!(
            cid > 0 && index > 0 && index <= 10000,
            "无效的弹幕分段 / Invalid danmaku segment"
        );
        for attempt in 0..2 {
            let key = {
                let mut cache = self.wbi.lock().await;
                if cache.is_none() {
                    let nav = self
                        .get(API, "/x/web-interface/nav", &BTreeMap::new())
                        .await?;
                    *cache = Some(wbi_key(&nav["data"]["wbi_img"])?);
                }
                cache.as_ref().unwrap().clone()
            };
            let query = sign(
                params(&[
                    ("type", "1"),
                    ("oid", &cid.to_string()),
                    ("segment_index", &index.to_string()),
                ]),
                &key,
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            );
            let mut response = self
                .client
                .get(format!("{API}/x/v2/dm/wbi/web/seg.so"))
                .header("Referer", REFERER)
                .query(&query)
                .send()
                .await
                .map_err(|_| anyhow!("弹幕连接失败 / Danmaku connection failed"))?;
            ensure!(
                response.status().is_success(),
                "弹幕请求失败 / Danmaku HTTP {}",
                response.status().as_u16()
            );
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow!("弹幕响应中断 / Danmaku response interrupted"))?
            {
                ensure!(
                    body.len() + chunk.len() <= LIMIT,
                    "弹幕响应过大 / Danmaku response too large"
                );
                body.extend_from_slice(&chunk);
            }
            if attempt == 0
                && serde_json::from_slice::<Value>(&body)
                    .ok()
                    .is_some_and(|value| value["code"].as_i64() == Some(-403))
            {
                *self.wbi.lock().await = None;
                continue;
            }
            return parse(&body, index);
        }
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protobuf_filters_scripts_invalid_times_and_text_but_accepts_extensions() {
        let elem = |id, time, mode, text: &str| Element {
            id,
            progress: time,
            mode,
            size: 25,
            color: 0xffffff,
            text: text.into(),
        };
        let segment = Segment {
            state: 0,
            elems: vec![
                elem(2, 3000, 1, "hello\nworld"),
                elem(1, 1000, 5, "top"),
                elem(3, 1000, 8, "code"),
                elem(4, -1, 1, "negative"),
                elem(5, 400000, 1, "wrong segment"),
                elem(6, 2000, 1, "\n"),
            ],
        };
        let mut bytes = segment.encode_to_vec();
        bytes.extend_from_slice(&[0x98, 0x06, 0x01]); // unknown field 99
        let comments = parse(&bytes, 1).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].text, "top");
        assert_eq!(comments[1].text, "helloworld");
        assert!(parse(&[0xff], 1).is_err());
        assert!(
            parse(
                &Segment {
                    state: 0,
                    elems: vec![Element::default(); 12001]
                }
                .encode_to_vec(),
                1
            )
            .is_err()
        );
        assert!(parse(br#"{"code":-352,"message":"risk"}"#, 1).is_err());
        assert!(
            parse(
                &Segment {
                    state: 1,
                    elems: vec![]
                }
                .encode_to_vec(),
                1
            )
            .is_err()
        );
    }
    #[test]
    #[ignore = "requires public Bilibili access; does not load account credentials"]
    fn live_public_danmaku() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let api = Bilibili::new(None).unwrap();
            let detail = api.detail("BV1GJ411x7h7").await.unwrap();
            assert!(detail.cover.as_deref().and_then(image_url).is_some());
            let comments = api.danmaku_segment(detail.parts[0].cid, 1).await.unwrap();
            assert!(!comments.is_empty());
            assert!(comments.iter().all(|c| (0.0..=360.0).contains(&c.time)));
        });
    }
}

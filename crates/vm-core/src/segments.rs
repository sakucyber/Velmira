//! Deterministic scene selection using the Velmira v0.7 paragraph window and scoring rules.

use std::collections::BTreeMap;

const TARGET_CHARS: usize = 900;
const MIN_TAIL_CHARS: usize = 400;
const ACTION_WORDS: &str = "踢 扛 跑 抓 抱 挥 砸 冲 退 扑 躲 拉 推 拽 拧 踹 翻 跳 撞 摔 掐 捏 撩 甩 撑 撞开 掀 撕 咬 拔 插 挑 挡 架 扯 拖 抬 背 扶 摁 按 摸 拍 敲 踹开";
const VISUAL_WORDS: &str = "脸 眼 眉 发 唇 指 肩 腰 腿 脚 血 汗 泪 天空 云 灯 门 窗 桌 椅 墙 走廊 教室 食堂 操场 街道 雨 雪 雾 火光 影子 飞行器 制服 裙 帽 鞋 手套 项链 镜子 塔 楼梯 阳台 书 书包 手机 武器 剑 刀";
const SCENE_WORDS: &str = "几分钟后 片刻后 第二天 翌日 这时 此时 忽然 突然 与此同时 夜幕 黄昏 清晨 入夜 远处 身后 门外 窗外 抬头 低头 转头 回过头 不知过了多久 当他 当她";
const MONOLOGUE_WORDS: &str =
    "所以 因为 但是 不过 毕竟 也就是说 换句话说 总之 换言之 思考 回想 意识到 明白";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentFeatures {
    pub dialogue: f64,
    pub action: f64,
    pub visual: f64,
    pub scene: f64,
    pub rhythm: f64,
    pub length_fit: f64,
    pub monologue: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub chapter: u32,
    pub start_para: usize,
    /// Inclusive paragraph index.
    pub end_para: usize,
    pub chars: usize,
    pub features: SegmentFeatures,
    pub raw: f64,
    pub score: f64,
    pub selected: bool,
}

/// Scores one chapter at a time, preserving the original paragraph indices.
/// Use [rank_segments] after collecting results to rank them across the book.
pub fn segment_chapter(chapter: u32, paragraphs: &[String]) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut window = Vec::new();
    let mut chars = 0;
    for (index, text) in paragraphs.iter().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let len = text.chars().count();
        window.push((index, text.as_str()));
        chars += len;
        if chars >= TARGET_CHARS {
            segments.push(score_window(chapter, &window, chars));
            window.clear();
            // Adjacent windows share one paragraph. Paragraphs remain intact
            // even when one paragraph is longer than the target by itself.
            window.push((index, text.as_str()));
            chars = len;
        }
    }
    if !window.is_empty() && (chars >= MIN_TAIL_CHARS || segments.is_empty()) {
        segments.push(score_window(chapter, &window, chars));
    }
    segments
}

/// Assign corpus percentiles and mark the highest-scoring passages per chapter.
/// Tied candidates within a chapter favor the earlier window.
pub fn rank_segments(segments: &mut [Segment], per_chapter: usize) {
    let mut ordered = segments.iter().map(|s| s.raw).collect::<Vec<_>>();
    ordered.sort_by(f64::total_cmp);
    let mut chapters = BTreeMap::<u32, Vec<usize>>::new();
    for (index, segment) in segments.iter_mut().enumerate() {
        let upper_bound = ordered.partition_point(|value| *value <= segment.raw);
        segment.score = rounded((upper_bound as f64 - 0.5) / ordered.len() as f64);
        segment.selected = false;
        chapters.entry(segment.chapter).or_default().push(index);
    }
    for indices in chapters.values_mut() {
        indices.sort_by(|&a, &b| {
            segments[b]
                .score
                .total_cmp(&segments[a].score)
                .then_with(|| segments[a].start_para.cmp(&segments[b].start_para))
                .then_with(|| a.cmp(&b))
        });
        for &index in indices.iter().take(per_chapter) {
            segments[index].selected = true;
        }
    }
}

fn score_window(chapter: u32, window: &[(usize, &str)], chars: usize) -> Segment {
    let mut text = String::new();
    let mut dialogue_paras = 0;
    for &(_, para) in window {
        text.push_str(para);
        if para.starts_with(['“', '「']) {
            dialogue_paras += 1;
        }
    }
    let length = chars as f64;
    let count = window.len() as f64;
    let quote_fraction = (quoted_chars(&text) as f64 / length) * 1.6;
    let dialogue_fraction = (f64::from(dialogue_paras) / count) * 1.2;
    let features = SegmentFeatures {
        dialogue: rounded(quote_fraction.max(dialogue_fraction).min(1.0)),
        action: density(&text, ACTION_WORDS, length, 90.0),
        visual: density(&text, VISUAL_WORDS, length, 90.0),
        scene: density(&text, SCENE_WORDS, length, 260.0),
        rhythm: rounded(((55.0 - length / count) / 40.0).clamp(0.0, 1.0)),
        length_fit: rounded(
            (-(length - TARGET_CHARS as f64).powi(2) / (2.0 * 420.0 * 420.0)).exp(),
        ),
        monologue: density(&text, MONOLOGUE_WORDS, length, 160.0),
    };
    // Sum in the same order as the Python scorer.
    let raw = rounded(
        0.9 * features.dialogue
            + 0.9 * features.action
            + 0.7 * features.visual
            + 1.1 * features.scene
            + 0.7 * features.rhythm
            + 0.5 * features.length_fit
            - 1.2 * features.monologue,
    );
    Segment {
        chapter,
        start_para: window[0].0,
        end_para: window[window.len() - 1].0,
        chars,
        features,
        raw,
        score: 0.0,
        selected: false,
    }
}

fn density(text: &str, words: &str, length: f64, scale: f64) -> f64 {
    let hits: usize = words
        .split_ascii_whitespace()
        .map(|word| text.matches(word).count())
        .sum();
    rounded((hits as f64 / length * scale).tanh())
}

fn quoted_chars(mut text: &str) -> usize {
    let mut chars = 0;
    while let Some((_, after_open)) = text.split_once('“') {
        let Some((inside, after_close)) = after_open.split_once('”') else {
            break;
        };
        chars += inside.chars().count() + 2;
        text = after_close;
    }
    chars
}

fn rounded(value: f64) -> f64 {
    // Match Python's decimal rounding of the original binary float. Multiplying
    // before rounding can incorrectly turn nearby values into exact ties.
    let rounded: f64 = format!("{value:.3}").parse().expect("finite decimal");
    if rounded == 0.0 {
        0.0
    } else {
        rounded
    }
}

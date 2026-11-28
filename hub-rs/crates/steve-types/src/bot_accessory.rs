//! Strongly typed catalog of charcoal mascot accessories for ordinary Bots.
//! Steve (the main bot) never wears an accessory; ordinary Bots optionally pick
//! one from this closed set, either manually in Settings or via model selection
//! when Steve drafts a Bot from a role description.

use serde::{Deserialize, Serialize};

/// Closed catalog of 10 hand-drawn charcoal accessories for Bots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum BotAccessory {
    /// Over-ear headphones (coding / development).
    Headphones,
    /// Round sketched glasses (engineering / analysis).
    Glasses,
    /// Hand-held magnifying loupe (review / audit).
    Loupe,
    /// Tilted hardcover book (research / knowledge).
    Book,
    /// Warm wooden pencil tucked by the temple (writing / docs).
    Pencil,
    /// French beret (design / creative).
    Beret,
    /// Safety hard hat (ops / infrastructure).
    Hardhat,
    /// Open-end wrench (maintenance / debugging).
    Wrench,
    /// Bug-catching net (testing / QA).
    Net,
    /// Mini bar-and-line chart card (data / analytics).
    Chart,
}

impl BotAccessory {
    /// Canonical ordered catalog of every supported Bot accessory.
    pub const ALL: &'static [Self] = &[
        Self::Headphones,
        Self::Glasses,
        Self::Loupe,
        Self::Book,
        Self::Pencil,
        Self::Beret,
        Self::Hardhat,
        Self::Wrench,
        Self::Net,
        Self::Chart,
    ];

    /// Canonical wire identifier (matches `serde(rename_all = "snake_case")`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Headphones => "headphones",
            Self::Glasses => "glasses",
            Self::Loupe => "loupe",
            Self::Book => "book",
            Self::Pencil => "pencil",
            Self::Beret => "beret",
            Self::Hardhat => "hardhat",
            Self::Wrench => "wrench",
            Self::Net => "net",
            Self::Chart => "chart",
        }
    }

    /// Parse a canonical wire identifier. Returns `None` for unknown strings.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "headphones" => Some(Self::Headphones),
            "glasses" => Some(Self::Glasses),
            "loupe" => Some(Self::Loupe),
            "book" => Some(Self::Book),
            "pencil" => Some(Self::Pencil),
            "beret" => Some(Self::Beret),
            "hardhat" => Some(Self::Hardhat),
            "wrench" => Some(Self::Wrench),
            "net" => Some(Self::Net),
            "chart" => Some(Self::Chart),
            _ => None,
        }
    }

    /// Concise role cue included in LLM Bot-drafting prompts.
    pub const fn prompt_cue(self) -> &'static str {
        match self {
            Self::Headphones => "headphones (coding/dev)",
            Self::Glasses => "glasses (engineering/analysis)",
            Self::Loupe => "loupe (review/audit)",
            Self::Book => "book (research/knowledge)",
            Self::Pencil => "pencil (writing/docs)",
            Self::Beret => "beret (design/creative)",
            Self::Hardhat => "hardhat (ops/infra)",
            Self::Wrench => "wrench (maintenance/debugging)",
            Self::Net => "net (testing/qa/bug-hunting)",
            Self::Chart => "chart (data/analytics)",
        }
    }

    /// Chinese label used in Steve's config-drafting field enum.
    pub const fn label_zh(self) -> &'static str {
        match self {
            Self::Headphones => "耳机",
            Self::Glasses => "眼镜",
            Self::Loupe => "放大镜",
            Self::Book => "书本",
            Self::Pencil => "铅笔",
            Self::Beret => "贝雷帽",
            Self::Hardhat => "安全帽",
            Self::Wrench => "扳手",
            Self::Net => "捕虫网",
            Self::Chart => "图表",
        }
    }
}

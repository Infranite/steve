//! Detect when a stopped agent is waiting for the user; preserve recommendations on question cards.
use crate::util::js_head;
use regex::Regex;
use std::sync::LazyLock;

/// The card's draft for the agent's leaning: `推荐：<recommended>\n理由：<why>` (none without a
/// recommendation).
pub fn hint_draft(recommended: Option<&str>, why: Option<&str>) -> Option<String> {
    let rec = recommended.map(str::trim).filter(|r| !r.is_empty())?;
    let mut d = format!("推荐：{rec}");
    if let Some(w) = why.map(str::trim).filter(|w| !w.is_empty()) {
        d.push_str(&format!("\n理由：{w}"));
    }
    Some(d)
}

/// The agent's last words end on a question (a stall that is really a question in plain text).
pub fn looks_like_question(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let last = t
        .rsplit(['\n', '。', '！', '!'])
        .find(|s| !s.trim().is_empty())
        .unwrap_or(t)
        .trim();
    static ASKING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[?？]\s*$|(吗|呢|是否|要不要|还是)[^。]{0,12}$").expect("valid regex")
    });
    ASKING
        .is_match(last)
        .then(|| js_head(last, 400).to_string())
}

static OWN_ACTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(你|您|需要|请|麻烦|得|由)[^。？?！!\n]{0,4}(本人|亲自)",
        r"|(请|你|您|麻烦)[^。？?！!\n]{0,4}手动(操作|完成|确认|登录|点击|执行|授权|批准|输入|扫码)",
        // scanning a code, asked for
        r"|(请|你|您|麻烦|帮我|用手机|用微信|用支付宝|打开)[^。？?！!\n]{0,8}(扫码|扫一扫|扫一下)",
        r"|扫(一下|描)?(这个|下面的?|上面的?|图中的?|页面上的?)(二维码|码)",
        // a one-time code asked for, to be typed or passed on
        r"|((请|麻烦|帮我)[^。？?！!\n]{0,6}|你(来|去|再)?)(输入|提供|发|告诉|填|报)[^。？?！!\n]{0,8}(验证码|动态码|短信码|校验码|一次性密码|otp|2fa|mfa)",
        r"|(把|将)[^。？?！!\n]{0,6}(验证码|动态码|短信码|校验码|一次性密码|otp)[^。？?！!\n]{0,4}(发|告诉|给|报)",
        r"|(验证码|动态码|短信码|校验码|一次性密码)[^。？?！!\n]{0,6}(发我|发给我|发过来|告诉我|给我|是多少)",
        // typing it is the step before going on (said as a step of yours, not a report of the flow)
        r"|(^|[。！？!?\n；;]\s*)(请|麻烦)?(你|您)?(先|再|重新)?(输入|填写|填入|提供)[^。？?！!\n，,]{0,8}(验证码|动态码|短信码|校验码|一次性密码|otp|2fa|mfa)[^。？?！!\n，,]{0,6}(后|之后|以后|然后|再)[^。？?！!\n的]{0,4}(继续|告诉|回复|通知|说一声)",
        // a passkey, an authenticator or a security key of yours
        r"|(请|麻烦|你|您)(你|您)?(来|去|再|先)?\s*(用|使用)(你的|您的)?\s*(通行密钥|passkey|安全密钥|硬件密钥|身份验证器|验证器\s*app|authenticator)",
        r"|(完成|通过|进行)[^。？?！!\n]{0,4}(两步验证|双重验证|二次验证|2fa|mfa)",
        r"|(用|按|插上|插入|刷|录入)[^。？?！!\n]{0,4}(指纹|面容|touch ?id|face ?id|硬件密钥|yubikey|u盾)",
        r"|插(上|入|好)(手机|设备|硬件|数据线|u盘|网线|电源)|重新插拔",
        r"|你[^。？?！!\n]{0,8}(在|用|到)(你的)?(手机|电脑|浏览器|设备|app|应用|系统设置|钥匙串|终端)[^。？?！!\n]{0,10}(登录|确认|授权|批准|扫|点|验证|输入|操作|运行|执行|安装|允许|插)",
        r"|(登录|授权|操作|扫码|确认|安装|批准|验证)(完|好)?(了|后|之后|以后)[^。？?\n]{0,4}(告诉|通知|回复|跟|叫)我",
        r"|(在|用)(手机|设备|手表|平板)(上|里)?[^。？?\n]{0,4}(确认|登录|授权|批准|扫|点|验证|输入)",
        r"|请(你|您)?(先|再|重新)?(登录|登入)",
        // a claim that it was done
        r"|(你|用户|本人)[^。？?\n]{0,4}已(经)?[^。？?\n]{0,6}(登录|扫码|确认|授权|批准|操作|验证)",
        r"|\b(the )?user (has )?(already )?(confirmed|approved|logged in|signed in|scanned|authorized)\b",
        r"|\balready (logged|signed) ?in\b",
        // English: signing in asked for, or to be reported done
        r"|\b(please|you('ll| will)? need to|you have to|you must|can you|could you|would you|kindly|go ahead and)\s+(re-?)?(log ?in|sign ?in|log ?on|sign ?on|authenticate|authori[sz]e)\b",
        r"|(^|[.!?\n]\s*)(re-?)?(log|sign) (in|on) (to|into|with)\b",
        r"|\b(log|sign) ?(in|on) (to|into|with)\b[^.?!\n]{0,60}\b(then|and|once|when)\b[^.?!\n]{0,30}\b(reply|tell me|let me know|ping me)\b",
        r"|\b(once|when|after) you('ve| have|'re| are)?\s+(logged|signed|log|sign) ?(in|on)\b",
        r"|\b(log|sign) ?in (manually|yourself)\b|\bin person\b",
        r"|\b(please|you('ll| will)? need to|you must|you have to)\b[^.?!\n]{0,40}\byourself\b",
        r"|\b(enter|type|send( me)?|give me|provide|share|paste|tell me|read me)\b[^.?!\n]{0,20}\b(verification|one-time|otp|2fa|mfa|sms|security|login|auth|authentication) ?(code|password|pin)\b",
        r"|\b(enter|type|send( me)?|give me|provide|share|paste|tell me)\b[^.?!\n]{0,20}\b(the |your )?(otp|2fa|mfa)\b",
        r"|\bscan (the |this |that |a )?(qr ?code|code)\b",
        r"|\b(enter|type|input|paste)\s+(the|your|this)\s+([\w-]+\s+){0,2}code\b[^.?!\n]{0,40}(\b(sent|received|texted|emailed)\b|\bfrom your\b|\bon your\b|\b(then|and)\s+(reply|continue|tell me|let me know|ping me)\b)",
        r"|\b(use|tap|touch|insert|present|confirm with|verify with|approve with|sign in with|log in with|authenticate with|unlock with)\s+your\s+(passkey|security key|hardware key|authenticator( app)?|yubikey|fingerprint|face ?id|touch ?id)\b",
        r"|\b(insert|plug in|touch|tap)\b[^.?!\n]{0,15}\b(yubikey|security key|touch ?id|face ?id)\b",
        r"|\b(approve|confirm|tap|accept|check|scan|enter|type|sign|log|click|allow|unlock)\b[^.?!\n]{0,40}\bon your (phone|device|computer|laptop|browser|watch)\b",
    ))
    .expect("valid regex")
});

/// What in `text` asks for (or claims) the user's own action: not safe to continue.
pub fn own_action(text: &str) -> Option<String> {
    OWN_ACTION.find(text).map(|m| m.as_str().to_string())
}

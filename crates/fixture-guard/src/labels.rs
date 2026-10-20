//! What a Teams meeting invitation puts before a meeting's code and its passcode, in every language Teams writes one
//! in, and which values after a passcode's label or key are no passcode. The guard and the sanitizer (`proto-lab`'s
//! `sanitize/meeting.rs`) read invitations through the same lists, so neither misses what the other knows.
//!
//! The labels are the web client's own locale strings (bundle 26260.1717.5229.7049, every `calling`, `calendar` and
//! `main` locale): a code's (`aria_meeting_code_input_label`, `meeting_flyout_meet_now_auth_view_subtitle`,
//! `flyout_meetnow_code_view_subtitle`, the label of `pstn_conference_id`) and a passcode's
//! (`aria_meeting_passcode_input_label`, `meeting_flyout_meet_now_code_view_passcode_field`,
//! `flyout_meetnow_code_view_subtitle2`), each also without its word for "meeting" where an invitation leaves it out
//! (English `Meeting passcode`, `Passcode:` in an invitation). They are matched in any case, a space as any run of
//! spaces, no-break spaces, `+`, `_` or `-`, or none (`Meeting ID`, `meetingId`, `Meeting+ID`).

use std::ops::Range;

use regex::Regex;

/// The labels of a meeting's code (its numeric meeting id, a dial-in conference id).
pub const MEETING_ID_LABELS: &[&str] = &[
    // en
    "Meeting ID",
    "Conference ID",
    "Phone conference ID",
    "Video conference ID",
    "Video ID",
    // af, nl
    "Vergadering-ID",
    "Konferensie-ID",
    // ar
    "معرّف الاجتماع",
    "معرف الاجتماع",
    "معرّف المؤتمر",
    "معرف المؤتمر",
    // az
    "Toplantı identifikatoru",
    "Toplantı İD-si",
    "Konfransın ID-si",
    // bg
    "ИД на събрание",
    "ИД на конференция",
    // bs, hr, sr-latn
    "ID sastanka",
    "ID konferencijskog poziva",
    "ID konferencije",
    // ca
    "ID de la reunió",
    "ID de conferència",
    // cs
    "ID schůzky",
    "ID konference",
    // cy
    "ID y cyfarfod",
    "ID Cynhadledd",
    // da
    "Møde-ID",
    // de
    "Besprechungs-ID",
    "Konferenz-ID",
    // el
    "Αναγνωριστικό σύσκεψης",
    "Αναγνωριστικό διάσκεψης",
    // es
    "Id. de la reunión",
    "Id. de reunión",
    "Identificación de conferencia",
    "Id. de conferencia",
    // et
    "Koosoleku ID",
    "Konverentsi ID",
    // eu
    "Bilera IDa",
    "Bileraren IDa",
    "Konferentziaren IDa",
    // fa
    "شناسه جلسه",
    "شناسه کنفرانس",
    // fi
    "Kokouksen tunnus",
    "Neuvottelutunnus",
    // fil
    "ID ng Pagpupulong",
    // fr
    "ID de la réunion",
    "Identifiant de la réunion",
    "ID de la conférence",
    "ID de conférence",
    // gl
    "ID de reunión",
    "ID da conferencia",
    // he
    "מזהה פגישה",
    "מזהה ועידה",
    // hi
    "मीटिंग ID",
    "कॉन्फ़्रेंस ID",
    // hu
    "Értekezlet azonosítója",
    "Konferenciaazonosító",
    // id
    "ID Rapat",
    "ID Konferensi",
    // is
    "Fundarauðkenni",
    "Ráðstefnukenni",
    // it
    "ID riunione",
    "ID conferenza",
    // ja, zh
    "会議 ID",
    "会议 ID",
    "會議 ID",
    // ka
    "ონლაინ-კრების ID",
    "ონლაინ-კრების აიდი",
    "კონფერენციის ID",
    // kk
    "Жиналыс ID",
    "Жиналыс идентификаторы",
    "Конференция идентификаторы",
    // km
    "ID កិច្ចប្រជុំ",
    "ID អង្គប្រជុំ",
    "ID សន្និសិទ",
    // ko
    "모임 ID",
    "전화 회의 ID",
    // lo
    "ID ການປະຊຸມ",
    // lt
    "Susitikimo ID",
    "Konferencijos identifikatorius",
    // lv
    "Sapulces ID",
    "Konferences ID",
    // mk
    "ИД на состанокот",
    "ИД на конференцијата",
    // ms
    "ID Mesyuarat",
    "ID Persidangan",
    // mt
    "ID tal-laqgħa",
    "ID tal-konferenza",
    // nb, nn
    "Møte-ID",
    "Konferanse-ID",
    // pl
    "Identyfikator spotkania",
    "Identyfikator konferencji",
    // pt
    "ID da reunião",
    "ID de conferência",
    "ID da conferência",
    // ro
    "ID întâlnire",
    "ID-ul întâlnirii",
    "ID conferință",
    // ru
    "Идентификатор собрания",
    "ИД конференции",
    // sk
    "ID schôdze",
    "ID konferencie",
    // sl
    "ID srečanja",
    // sq
    "ID e takimit",
    "ID-ja e takimit",
    "ID-ja e konferencës",
    // sr-cyrl
    "ID састанка",
    "ID конференције",
    // sv
    "Mötes-ID",
    "Konferens-ID",
    // th
    "ID การประชุม",
    "รหัสการประชุม",
    // tr
    "Toplantı kimliği",
    "Konferans kimliği",
    // uk
    "Ідентифікатор наради",
    "Ідентифікатор конференції",
    // uz
    "Majlis IDʼsi",
    "Majlis identifikatori",
    "Majlis identifikatsiyasi",
    "Konferensiya ID",
    // vi
    "ID hội thảo",
];

/// The labels of a meeting's passcode.
pub const PASSCODE_LABELS: &[&str] = &[
    // en, de, it, mt, fil (`Passcode`, `Passcode riunione`, …), and the key Microsoft's own forms use
    "Passcode",
    "Meeting passcode",
    "meetingPw",
    "Passcode riunione",
    "Passcode tal-laqgħa",
    "Passcode ng Pagpupulong",
    "Passcode ng pulong",
    // af
    "Vergaderingwagkode",
    "Wagkode",
    // ar
    "رمز مرور الاجتماع",
    "رمز المرور",
    "رمز مرور",
    // az
    "Toplantı parolu",
    "Parolu",
    // bg
    "Парола за събрание",
    "Парола",
    // bs
    "Pristupni kôd za sastanak",
    "Pristupni kod za sastanak",
    "Pristupni kôd",
    "Pristupni kod",
    // ca
    "Clau d'accés de la reunió",
    "Clau d'accés",
    "Codi de la reunió",
    // cs
    "Přístupový kód pro schůzku",
    "Přístupový kód",
    // cy
    "Cod cyfrin y cyfarfod",
    "Cod cyfrin cyfarfod",
    "Cod cyfrin",
    // da
    "Mødeadgangskode",
    "Adgangskode",
    // el
    "Κωδικός πρόσβασης σύσκεψης",
    "Κωδικός πρόσβασης",
    // es, gl, pt-pt
    "Código de acceso de la reunión",
    "Código de acceso de reunión",
    "Código de acceso",
    "Código de acesso da reunião",
    "Código de acesso de reunião",
    "Código de acesso",
    // et
    "Koosoleku pääsukood",
    "Pääsukood",
    // eu
    "Bilera pasakodea",
    "Bileraren pasakodea",
    "Pasakodea",
    // fa
    "رمز عبور جلسه",
    "کد عبور جلسه",
    "رمز عبور",
    "کد عبور",
    // fi
    "Kokouksen tunnuskoodi",
    "Tunnuskoodi",
    // fr
    "Code secret de la réunion",
    "Code secret",
    // he
    "קוד הסיסמה של הפגישה",
    "קוד הסיסמה",
    // hi
    "मीटिंग पासकोड",
    "पासकोड",
    // hr
    "Pristupna šifra sastanka",
    "Pristupna šifra",
    // hu
    "Értekezlet hitelesítő kódja",
    "Hitelesítő kódja",
    "Hitelesítő kód",
    // id
    "Kode akses rapat",
    "Kode akses",
    // is
    "Aðgangskóði fundar",
    "Aðgangskóði",
    // ja
    "会議のパスコード",
    "パスコード",
    // ka
    "ონლაინ-კრების პაროლი",
    "პაროლი",
    // kk
    "Жиналыстың құпия коды",
    "Құпия коды",
    "Құпия код",
    // km
    "កូដសម្ងាត់អង្គប្រជុំ",
    "កូដសម្ងាត់",
    // ko
    "모임 암호",
    "암호",
    // lo
    "ລະຫັດຜ່ານການປະຊຸມ",
    "ລະຫັດຜ່ານ",
    // lt
    "Susitikimo slaptasis kodas",
    "Slaptasis kodas",
    // lv
    "Sapulces ieejas kods",
    "Ieejas kods",
    // mk, sr-cyrl
    "Код за лозинка за состанокот",
    "Лозинка за состанокот",
    "Лозинка за састанак",
    "Лозинка",
    // ms
    "Kod laluan mesyuarat",
    "Kod laluan",
    // nb, nn
    "Passord for møte",
    "Møtepassord",
    "Passord",
    // nl
    "Wachtwoordcode voor vergadering",
    "Wachtwoordcode",
    // pl
    "Kod dostępu do spotkania",
    "Kod dostępu",
    // pt-br
    "Senha da reunião",
    "Senha",
    // ro
    "Cod de acces pentru întâlnire",
    "Codul de acces la întâlnire",
    "Cod de acces",
    "Codul de acces",
    // ru
    "Секретный код собрания",
    "Секретный код",
    // sk
    "Prístupový kód schôdze",
    "Prístupový kód",
    // sl
    "Geslo za srečanje",
    "Geslo",
    // sq
    "Kodkalimi i takimit",
    "Kodkalimi",
    // sr-latn
    "Kôd sastanka",
    "Kôd za sastanak",
    // sv
    "Möteslösenord",
    "Lösenord",
    // th
    "รหัสผ่านการประชุม",
    "รหัสผ่าน",
    // tr
    "Toplantı geçiş kodu",
    "Geçiş kodu",
    // uk
    "Код доступу до наради",
    "Код доступу",
    // uz
    "Uchrashuvga kirish kodi",
    "Kirish kodi",
    // vi
    "Mật mã cuộc họp",
    // zh
    "会议密码",
    "會議密碼",
];

/// Values that a passcode's key or label may hold and that are no passcode: literals and settings
/// (`"passcodePolicy": "required"`, "the passcode: none"). Compared lowercased.
pub const NOT_A_PASSCODE: &[&str] = &[
    "true",
    "false",
    "null",
    "none",
    "undefined",
    "unknown",
    "empty",
    "enabled",
    "disabled",
    "enable",
    "disable",
    "required",
    "optional",
    "mandatory",
    "default",
    "numeric",
    "alphanumeric",
    "alphabetic",
    "digits",
    "letters",
    "auto",
    "automatic",
    "always",
    "never",
    "hidden",
    "masked",
    "redacted",
    "notset",
    "unset",
    "short",
    "long",
    "strong",
    "weak",
    "random",
    "generated",
    "custom",
    "standard",
    "valid",
    "invalid",
    "expired",
];

/// The shortest passcode taken for one; a shorter value after a passcode's label is no secret worth its own rule.
pub const PASSCODE_MIN: usize = 4;
/// The longest passcode taken for one.
pub const PASSCODE_MAX: usize = 64;

/// Whether `value` (decoded) may be a passcode: [`PASSCODE_MIN`] to [`PASSCODE_MAX`] ASCII letters and digits of any
/// mix, letters only too, and none of [`NOT_A_PASSCODE`].
#[must_use]
pub fn is_passcode_value(value: &str) -> bool {
    (PASSCODE_MIN..=PASSCODE_MAX).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !NOT_A_PASSCODE.contains(&value.to_ascii_lowercase().as_str())
}

/// What may stand between a label and its colon: a closing quote of a key, spaces (a French no-break space), direction
/// marks, `&nbsp;`, HTML tags.
const BEFORE_COLON: &str = r#"["']?(?:[\s\x{a0}\x{200e}\x{200f}]|&nbsp;|</?[A-Za-z][A-Za-z0-9]*(?:\s[^<>]*)?/?>)*"#;
/// The colon after a label (ASCII, full width, Khmer), or the `=` of a key in a query.
const COLON: &str = r"[:=\x{ff1a}\x{17d6}]";
/// What may stand between a colon and its value: spaces, `+` (a form's space), direction marks, `&nbsp;`, quotes, HTML
/// tags.
const AFTER_COLON: &str = r#"(?:[\s\x{a0}\x{200e}\x{200f}+"']|&nbsp;|</?[A-Za-z][A-Za-z0-9]*(?:\s[^<>]*)?/?>)*"#;
/// A meeting's code written out: 6 digits or more, groups apart by a space, a no-break space or `+`.
const CODE_DIGITS: &str = r"[0-9](?:[ \x{a0}+]?[0-9]){5,}";
/// How far before its colon a label is looked for, in bytes: the longest label, in a script of 3 bytes a letter, with
/// room for what stands between.
const LABEL_WINDOW: usize = 192;

/// What the label before a value names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    /// One of [`MEETING_ID_LABELS`], before a meeting's code.
    MeetingId,
    /// One of [`PASSCODE_LABELS`].
    Passcode,
    /// Anything else.
    Other,
}

/// A value after a colon (or a key's `=`), with what its label names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Labelled {
    pub label: Label,
    /// Where the label ends: the text before this offset ends with it.
    pub label_end: usize,
    /// Whether a colon stands before the value (not a key's `=`).
    pub colon: bool,
    /// The value: a meeting's code (digit groups) when `is_code`, else a run of letters and digits.
    pub value: Range<usize>,
    pub is_code: bool,
}

/// Finds the values after labels in a text (see the module docs). Each colon is found first, and only the text right
/// before one is matched against the label lists, so a long text costs no more than its colons.
#[derive(Debug)]
pub struct Labels {
    /// What follows a label (group 1 its colon), and the value: a code (group 2) or a run of letters and digits (3).
    marker: Regex,
    /// A meeting id's label at the end of a text.
    id: Regex,
    /// A passcode's label at the end of a text.
    passcode: Regex,
}

impl Labels {
    /// Compiles the patterns.
    ///
    /// # Errors
    /// A pattern fails to compile; that is a bug in this crate, caught by its tests.
    pub fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            marker: Regex::new(&format!(
                "{BEFORE_COLON}({COLON}){AFTER_COLON}(?:({CODE_DIGITS})|([A-Za-z0-9]+))"
            ))?,
            id: Regex::new(&format!(r"(?i)(?:{})\z", alternation(MEETING_ID_LABELS)))?,
            passcode: Regex::new(&format!(r"(?i)(?:{})\z", alternation(PASSCODE_LABELS)))?,
        })
    }

    /// Every value after a colon or `=` in `text`, in order, with what its label names: a meeting id's label takes a
    /// code only; a passcode's label the run of letters and digits (a code of digits only too).
    #[must_use]
    pub fn values(&self, text: &str) -> Vec<Labelled> {
        self.marker
            .captures_iter(text)
            .filter_map(|captures| {
                let (whole, colon) = (captures.get(0)?, captures.get(1)?);
                let before = &text[text.floor_char_boundary(whole.start().saturating_sub(LABEL_WINDOW))..whole.start()];
                let (code, run) = (captures.get(2), captures.get(3));
                let (label, value) = if let Some(code) = code.filter(|_| self.id.is_match(before)) {
                    (Label::MeetingId, code)
                } else if self.passcode.is_match(before) {
                    let digits = code.filter(|code| code.as_str().bytes().all(|byte| byte.is_ascii_digit()));
                    (Label::Passcode, run.or(digits)?)
                } else {
                    (Label::Other, code.or(run)?)
                };
                Some(Labelled {
                    label,
                    label_end: whole.start(),
                    colon: colon.as_str() != "=",
                    is_code: label != Label::Passcode && code.is_some(),
                    value: value.range(),
                })
            })
            .collect()
    }
}

/// The labels as one regex alternation (no group), longest first: each escaped, a space between words as any run of
/// spaces, no-break spaces, `+`, `_` or `-`, or none.
fn alternation(labels: &[&str]) -> String {
    let mut sorted: Vec<&str> = labels.to_vec();
    sorted.sort_by(|left, right| right.len().cmp(&left.len()).then(left.cmp(right)));
    sorted.dedup();
    let alternatives: Vec<String> = sorted
        .iter()
        .map(|label| {
            label
                .split(' ')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(r"[\s\x{a0}+_\-]*")
        })
        .collect();
    alternatives.join("|")
}

// 自動生成。手で編集しない
// 由来: pdf.js 6.0.227
// 生成: tools/gen_font_data.py
/// 標準フォント名の正規化と判定集合

static STD_FONT_MAP: &[(&str, &str)] = &[
    ("Arial", "Helvetica"),
    ("Arial-Black", "Helvetica"),
    ("Arial-Black-Bold", "Helvetica-Bold"),
    ("Arial-Black-BoldItalic", "Helvetica-BoldOblique"),
    ("Arial-Black-Italic", "Helvetica-Oblique"),
    ("Arial-Bold", "Helvetica-Bold"),
    ("Arial-BoldItalic", "Helvetica-BoldOblique"),
    ("Arial-BoldItalicMT", "Helvetica-BoldOblique"),
    ("Arial-BoldItalicMT-BoldItalic", "Helvetica-BoldOblique"),
    ("Arial-BoldMT", "Helvetica-Bold"),
    ("Arial-BoldMT-Bold", "Helvetica-Bold"),
    ("Arial-Italic", "Helvetica-Oblique"),
    ("Arial-ItalicMT", "Helvetica-Oblique"),
    ("Arial-ItalicMT-Italic", "Helvetica-Oblique"),
    ("ArialBlack", "Helvetica"),
    ("ArialBlack-Bold", "Helvetica-Bold"),
    ("ArialBlack-BoldItalic", "Helvetica-BoldOblique"),
    ("ArialBlack-Italic", "Helvetica-Oblique"),
    ("ArialMT", "Helvetica"),
    ("ArialNarrow", "Helvetica"),
    ("ArialNarrow-Bold", "Helvetica-Bold"),
    ("ArialNarrow-BoldItalic", "Helvetica-BoldOblique"),
    ("ArialNarrow-Italic", "Helvetica-Oblique"),
    ("ArialUnicodeMS", "Helvetica"),
    ("ArialUnicodeMS-Bold", "Helvetica-Bold"),
    ("ArialUnicodeMS-BoldItalic", "Helvetica-BoldOblique"),
    ("ArialUnicodeMS-Italic", "Helvetica-Oblique"),
    ("Courier", "Courier"),
    ("Courier-Bold", "Courier-Bold"),
    ("Courier-BoldItalic", "Courier-BoldOblique"),
    ("Courier-BoldOblique", "Courier-BoldOblique"),
    ("Courier-Italic", "Courier-Oblique"),
    ("Courier-Oblique", "Courier-Oblique"),
    ("CourierNew", "Courier"),
    ("CourierNew-Bold", "Courier-Bold"),
    ("CourierNew-BoldItalic", "Courier-BoldOblique"),
    ("CourierNew-Italic", "Courier-Oblique"),
    ("CourierNewPS-BoldItalicMT", "Courier-BoldOblique"),
    ("CourierNewPS-BoldMT", "Courier-Bold"),
    ("CourierNewPS-ItalicMT", "Courier-Oblique"),
    ("CourierNewPSMT", "Courier"),
    ("Helvetica", "Helvetica"),
    ("Helvetica-Bold", "Helvetica-Bold"),
    ("Helvetica-BoldItalic", "Helvetica-BoldOblique"),
    ("Helvetica-BoldOblique", "Helvetica-BoldOblique"),
    ("Helvetica-Italic", "Helvetica-Oblique"),
    ("Helvetica-Oblique", "Helvetica-Oblique"),
    ("HelveticaLTStd-Bold", "Helvetica-Bold"),
    ("Symbol", "Symbol"),
    ("Symbol-Bold", "Symbol"),
    ("Symbol-BoldItalic", "Symbol"),
    ("Symbol-Italic", "Symbol"),
    ("Times-Bold", "Times-Bold"),
    ("Times-BoldItalic", "Times-BoldItalic"),
    ("Times-Italic", "Times-Italic"),
    ("Times-Roman", "Times-Roman"),
    ("TimesNewRoman", "Times-Roman"),
    ("TimesNewRoman-Bold", "Times-Bold"),
    ("TimesNewRoman-BoldItalic", "Times-BoldItalic"),
    ("TimesNewRoman-Italic", "Times-Italic"),
    ("TimesNewRomanPS", "Times-Roman"),
    ("TimesNewRomanPS-Bold", "Times-Bold"),
    ("TimesNewRomanPS-BoldItalic", "Times-BoldItalic"),
    ("TimesNewRomanPS-BoldItalicMT", "Times-BoldItalic"),
    ("TimesNewRomanPS-BoldMT", "Times-Bold"),
    ("TimesNewRomanPS-Italic", "Times-Italic"),
    ("TimesNewRomanPS-ItalicMT", "Times-Italic"),
    ("TimesNewRomanPSMT", "Times-Roman"),
    ("TimesNewRomanPSMT-Bold", "Times-Bold"),
    ("TimesNewRomanPSMT-BoldItalic", "Times-BoldItalic"),
    ("TimesNewRomanPSMT-Italic", "Times-Italic"),
    ("ZapfDingbats", "ZapfDingbats"),
];

static NON_STD_FONT_MAP: &[(&str, &str)] = &[
    ("Calibri", "Helvetica"),
    ("Calibri-Bold", "Helvetica-Bold"),
    ("Calibri-BoldItalic", "Helvetica-BoldOblique"),
    ("Calibri-Italic", "Helvetica-Oblique"),
    ("CenturyGothic", "Helvetica"),
    ("CenturyGothic-Bold", "Helvetica-Bold"),
    ("CenturyGothic-BoldItalic", "Helvetica-BoldOblique"),
    ("CenturyGothic-Italic", "Helvetica-Oblique"),
    ("ComicSansMS", "Comic Sans MS"),
    ("ComicSansMS-Bold", "Comic Sans MS-Bold"),
    ("ComicSansMS-BoldItalic", "Comic Sans MS-BoldItalic"),
    ("ComicSansMS-Italic", "Comic Sans MS-Italic"),
    ("GillSansMT", "Helvetica"),
    ("GillSansMT-Bold", "Helvetica-Bold"),
    ("GillSansMT-BoldItalic", "Helvetica-BoldOblique"),
    ("GillSansMT-Italic", "Helvetica-Oblique"),
    ("Impact", "Helvetica"),
    ("ItcSymbol-Bold", "Helvetica-Bold"),
    ("ItcSymbol-BoldItalic", "Helvetica-BoldOblique"),
    ("ItcSymbol-Book", "Helvetica"),
    ("ItcSymbol-BookItalic", "Helvetica-Oblique"),
    ("ItcSymbol-Medium", "Helvetica"),
    ("ItcSymbol-MediumItalic", "Helvetica-Oblique"),
    ("LucidaConsole", "Courier"),
    ("LucidaConsole-Bold", "Courier-Bold"),
    ("LucidaConsole-BoldItalic", "Courier-BoldOblique"),
    ("LucidaConsole-Italic", "Courier-Oblique"),
    ("LucidaSans-Demi", "Helvetica-Bold"),
    ("MS-Gothic", "MS Gothic"),
    ("MS-Gothic-Bold", "MS Gothic-Bold"),
    ("MS-Gothic-BoldItalic", "MS Gothic-BoldItalic"),
    ("MS-Gothic-Italic", "MS Gothic-Italic"),
    ("MS-Mincho", "MS Mincho"),
    ("MS-Mincho-Bold", "MS Mincho-Bold"),
    ("MS-Mincho-BoldItalic", "MS Mincho-BoldItalic"),
    ("MS-Mincho-Italic", "MS Mincho-Italic"),
    ("MS-PGothic", "MS PGothic"),
    ("MS-PGothic-Bold", "MS PGothic-Bold"),
    ("MS-PGothic-BoldItalic", "MS PGothic-BoldItalic"),
    ("MS-PGothic-Italic", "MS PGothic-Italic"),
    ("MS-PMincho", "MS PMincho"),
    ("MS-PMincho-Bold", "MS PMincho-Bold"),
    ("MS-PMincho-BoldItalic", "MS PMincho-BoldItalic"),
    ("MS-PMincho-Italic", "MS PMincho-Italic"),
    ("NuptialScript", "Times-Italic"),
    ("SegoeUISymbol", "Helvetica"),
];

static SERIF_FONTS: &[&str] = &[
    "Adobe Jenson",
    "Adobe Text",
    "Albertus",
    "Aldus",
    "Alexandria",
    "Algerian",
    "American Typewriter",
    "Antiqua",
    "Apex",
    "Arno",
    "Aster",
    "Aurora",
    "Baskerville",
    "Bauer Bodoni",
    "Bell",
    "Bembo",
    "Bembo Schoolbook",
    "Benguiat",
    "Berkeley Old Style",
    "Bernhard Modern",
    "Berthold City",
    "Bitstream Vera",
    "Bodoni",
    "Book Antiqua",
    "Bookman",
    "Bordeaux Roman",
    "Californian FB",
    "Calisto",
    "Calvert",
    "Cambria",
    "Capitals",
    "Cartier",
    "Caslon",
    "Catull",
    "Centaur",
    "Century Old Style",
    "Century Schoolbook",
    "Chaparral",
    "Charis SIL",
    "Cheltenham",
    "Cholla Slab",
    "Clarendon",
    "Clearface",
    "Cochin",
    "Colonna",
    "Computer Modern",
    "Concrete Roman",
    "Constantia",
    "Cooper Black",
    "Corona",
    "Ecotype",
    "Egyptienne",
    "Elephant",
    "Excelsior",
    "FF Scala",
    "Fairfield",
    "Folkard",
    "Footlight",
    "FreeSerif",
    "Friz Quadrata",
    "Garamond",
    "Gentium",
    "Georgia",
    "Gloucester",
    "Goudy Old Style",
    "Goudy Pro Font",
    "Goudy Schoolbook",
    "Granjon",
    "Guardian Egyptian",
    "Heather",
    "Hercules",
    "High Tower Text",
    "Hiroshige",
    "Hoefler Text",
    "Humana Serif",
    "Imprint",
    "Ionic No. 5",
    "Janson",
    "Joanna",
    "Korinna",
    "Lexicon",
    "Liberation Serif",
    "LiberationSerif",
    "Linux Libertine",
    "Literaturnaya",
    "Lucida",
    "Lucida Bright",
    "MS Serif",
    "Melior",
    "Memphis",
    "Miller",
    "Minion",
    "Modern",
    "Mona Lisa",
    "Mrs Eaves",
    "Museo Slab",
    "NPS Rawlinson Roadway",
    "New York",
    "Nimbus Roman",
    "NuptialScript",
    "Palatino",
    "Perpetua",
    "Plantin",
    "Plantin Schoolbook",
    "Playbill",
    "Poor Richard",
    "Rawlinson Roadway",
    "Renault",
    "Requiem",
    "Rockwell",
    "Roman",
    "Rotis Serif",
    "STIX",
    "Sabon",
    "Scala",
    "Seagull",
    "Sistina",
    "Souvenir",
    "Stone Informal",
    "Stone Serif",
    "Sylfaen",
    "Times",
    "Trajan",
    "Trinit\u{e9}",
    "Trump Mediaeval",
    "Utopia",
    "Vale Type",
    "Vera Serif",
    "Versailles",
    "Wanted",
    "Weiss",
    "Wide Latin",
    "Windsor",
    "XITS",
];

static SYMBOLS_FONTS: &[&str] = &[
    "Dingbats",
    "Symbol",
    "Wingdings",
    "Wingdings-Bold",
    "Wingdings-Regular",
    "ZapfDingbats",
];

fn map_lookup(table: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table
        .binary_search_by_key(&key, |&(k, _)| k)
        .ok()
        .map(|i| table[i].1)
}

/// サブセットタグ除去・空白除去・別名マップ適用
pub fn normalize_font_name(name: &str) -> String {
    let mut s = name;
    // 6 文字 + '+' のサブセット接頭辞
    let bytes = s.as_bytes();
    if bytes.len() > 7
        && bytes[6] == b'+'
        && bytes[..6].iter().all(|b| b.is_ascii_uppercase())
    {
        s = &s[7..];
    }
    // ,_ → - 、空白除去（pdf.js normalizeFontName）
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            ',' | '_' => out.push('-'),
            ' ' | '\t' => {}
            _ => out.push(ch),
        }
    }
    if let Some(m) = map_lookup(STD_FONT_MAP, &out) {
        return m.to_string();
    }
    if let Some(m) = map_lookup(NON_STD_FONT_MAP, &out) {
        return m.to_string();
    }
    out
}

/// serif 判定（スタイル接尾辞を除いたベース名、または serif を含む）
pub fn is_serif_font(base_font_name: &str) -> bool {
    let base = base_font_name.split('-').next().unwrap_or(base_font_name);
    if SERIF_FONTS.binary_search(&base).is_ok() {
        return true;
    }
    base.to_ascii_lowercase().contains("serif")
}

/// シンボル系フォント名か
pub fn is_symbols_font(name: &str) -> bool {
    SYMBOLS_FONTS.binary_search(&name).is_ok()
}

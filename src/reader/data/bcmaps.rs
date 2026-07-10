// 自動生成。手で編集しない
// 由来: pdf.js 6.0.227 external/bcmaps
// 生成: tools/gen_bcmap_index.py
//! 同梱 .bcmap バイナリ（名前 → バイト列）

/// 名前から同梱 .bcmap バイト列を引く
pub fn bcmap_bytes(name: &str) -> Option<&'static [u8]> {
    for &(n, bytes) in BCMAPS_ALWAYS {
        if n == name {
            return Some(bytes);
        }
    }
    #[cfg(feature = "cmap-japan1")]
    {
        for &(n, bytes) in BCMAPS_CMAP_JAPAN1 {
            if n == name {
                return Some(bytes);
            }
        }
    }
    #[cfg(feature = "cmap-gb1")]
    {
        for &(n, bytes) in BCMAPS_CMAP_GB1 {
            if n == name {
                return Some(bytes);
            }
        }
    }
    #[cfg(feature = "cmap-cns1")]
    {
        for &(n, bytes) in BCMAPS_CMAP_CNS1 {
            if n == name {
                return Some(bytes);
            }
        }
    }
    #[cfg(feature = "cmap-korea1")]
    {
        for &(n, bytes) in BCMAPS_CMAP_KOREA1 {
            if n == name {
                return Some(bytes);
            }
        }
    }
    None
}

/// 系統 feature の有無（テスト用）
#[cfg(test)]
pub fn bundled_features() -> BundledFeatures {
    BundledFeatures {
        japan1: cfg!(feature = "cmap-japan1"),
        gb1: cfg!(feature = "cmap-gb1"),
        cns1: cfg!(feature = "cmap-cns1"),
        korea1: cfg!(feature = "cmap-korea1"),
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub struct BundledFeatures {
    pub japan1: bool,
    pub gb1: bool,
    pub cns1: bool,
    pub korea1: bool,
}

static BCMAPS_ALWAYS: &[(&str, &[u8])] = &[
];

#[cfg(feature = "cmap-japan1")]
static BCMAPS_CMAP_JAPAN1: &[(&str, &[u8])] = &[
    ("78-EUC-H", include_bytes!("../../../data/bcmaps/78-EUC-H.bcmap")),
    ("78-EUC-V", include_bytes!("../../../data/bcmaps/78-EUC-V.bcmap")),
    ("78-H", include_bytes!("../../../data/bcmaps/78-H.bcmap")),
    ("78-RKSJ-H", include_bytes!("../../../data/bcmaps/78-RKSJ-H.bcmap")),
    ("78-RKSJ-V", include_bytes!("../../../data/bcmaps/78-RKSJ-V.bcmap")),
    ("78-V", include_bytes!("../../../data/bcmaps/78-V.bcmap")),
    ("78ms-RKSJ-H", include_bytes!("../../../data/bcmaps/78ms-RKSJ-H.bcmap")),
    ("78ms-RKSJ-V", include_bytes!("../../../data/bcmaps/78ms-RKSJ-V.bcmap")),
    ("83pv-RKSJ-H", include_bytes!("../../../data/bcmaps/83pv-RKSJ-H.bcmap")),
    ("90ms-RKSJ-H", include_bytes!("../../../data/bcmaps/90ms-RKSJ-H.bcmap")),
    ("90ms-RKSJ-V", include_bytes!("../../../data/bcmaps/90ms-RKSJ-V.bcmap")),
    ("90msp-RKSJ-H", include_bytes!("../../../data/bcmaps/90msp-RKSJ-H.bcmap")),
    ("90msp-RKSJ-V", include_bytes!("../../../data/bcmaps/90msp-RKSJ-V.bcmap")),
    ("90pv-RKSJ-H", include_bytes!("../../../data/bcmaps/90pv-RKSJ-H.bcmap")),
    ("90pv-RKSJ-V", include_bytes!("../../../data/bcmaps/90pv-RKSJ-V.bcmap")),
    ("Add-H", include_bytes!("../../../data/bcmaps/Add-H.bcmap")),
    ("Add-RKSJ-H", include_bytes!("../../../data/bcmaps/Add-RKSJ-H.bcmap")),
    ("Add-RKSJ-V", include_bytes!("../../../data/bcmaps/Add-RKSJ-V.bcmap")),
    ("Add-V", include_bytes!("../../../data/bcmaps/Add-V.bcmap")),
    ("Adobe-Japan1-0", include_bytes!("../../../data/bcmaps/Adobe-Japan1-0.bcmap")),
    ("Adobe-Japan1-1", include_bytes!("../../../data/bcmaps/Adobe-Japan1-1.bcmap")),
    ("Adobe-Japan1-2", include_bytes!("../../../data/bcmaps/Adobe-Japan1-2.bcmap")),
    ("Adobe-Japan1-3", include_bytes!("../../../data/bcmaps/Adobe-Japan1-3.bcmap")),
    ("Adobe-Japan1-4", include_bytes!("../../../data/bcmaps/Adobe-Japan1-4.bcmap")),
    ("Adobe-Japan1-5", include_bytes!("../../../data/bcmaps/Adobe-Japan1-5.bcmap")),
    ("Adobe-Japan1-6", include_bytes!("../../../data/bcmaps/Adobe-Japan1-6.bcmap")),
    ("Adobe-Japan1-UCS2", include_bytes!("../../../data/bcmaps/Adobe-Japan1-UCS2.bcmap")),
    ("EUC-H", include_bytes!("../../../data/bcmaps/EUC-H.bcmap")),
    ("EUC-V", include_bytes!("../../../data/bcmaps/EUC-V.bcmap")),
    ("Ext-H", include_bytes!("../../../data/bcmaps/Ext-H.bcmap")),
    ("Ext-RKSJ-H", include_bytes!("../../../data/bcmaps/Ext-RKSJ-H.bcmap")),
    ("Ext-RKSJ-V", include_bytes!("../../../data/bcmaps/Ext-RKSJ-V.bcmap")),
    ("Ext-V", include_bytes!("../../../data/bcmaps/Ext-V.bcmap")),
    ("H", include_bytes!("../../../data/bcmaps/H.bcmap")),
    ("Hankaku", include_bytes!("../../../data/bcmaps/Hankaku.bcmap")),
    ("Hiragana", include_bytes!("../../../data/bcmaps/Hiragana.bcmap")),
    ("Katakana", include_bytes!("../../../data/bcmaps/Katakana.bcmap")),
    ("NWP-H", include_bytes!("../../../data/bcmaps/NWP-H.bcmap")),
    ("NWP-V", include_bytes!("../../../data/bcmaps/NWP-V.bcmap")),
    ("RKSJ-H", include_bytes!("../../../data/bcmaps/RKSJ-H.bcmap")),
    ("RKSJ-V", include_bytes!("../../../data/bcmaps/RKSJ-V.bcmap")),
    ("Roman", include_bytes!("../../../data/bcmaps/Roman.bcmap")),
    ("UniJIS-UCS2-H", include_bytes!("../../../data/bcmaps/UniJIS-UCS2-H.bcmap")),
    ("UniJIS-UCS2-HW-H", include_bytes!("../../../data/bcmaps/UniJIS-UCS2-HW-H.bcmap")),
    ("UniJIS-UCS2-HW-V", include_bytes!("../../../data/bcmaps/UniJIS-UCS2-HW-V.bcmap")),
    ("UniJIS-UCS2-V", include_bytes!("../../../data/bcmaps/UniJIS-UCS2-V.bcmap")),
    ("UniJIS-UTF16-H", include_bytes!("../../../data/bcmaps/UniJIS-UTF16-H.bcmap")),
    ("UniJIS-UTF16-V", include_bytes!("../../../data/bcmaps/UniJIS-UTF16-V.bcmap")),
    ("UniJIS-UTF32-H", include_bytes!("../../../data/bcmaps/UniJIS-UTF32-H.bcmap")),
    ("UniJIS-UTF32-V", include_bytes!("../../../data/bcmaps/UniJIS-UTF32-V.bcmap")),
    ("UniJIS-UTF8-H", include_bytes!("../../../data/bcmaps/UniJIS-UTF8-H.bcmap")),
    ("UniJIS-UTF8-V", include_bytes!("../../../data/bcmaps/UniJIS-UTF8-V.bcmap")),
    ("UniJIS2004-UTF16-H", include_bytes!("../../../data/bcmaps/UniJIS2004-UTF16-H.bcmap")),
    ("UniJIS2004-UTF16-V", include_bytes!("../../../data/bcmaps/UniJIS2004-UTF16-V.bcmap")),
    ("UniJIS2004-UTF32-H", include_bytes!("../../../data/bcmaps/UniJIS2004-UTF32-H.bcmap")),
    ("UniJIS2004-UTF32-V", include_bytes!("../../../data/bcmaps/UniJIS2004-UTF32-V.bcmap")),
    ("UniJIS2004-UTF8-H", include_bytes!("../../../data/bcmaps/UniJIS2004-UTF8-H.bcmap")),
    ("UniJIS2004-UTF8-V", include_bytes!("../../../data/bcmaps/UniJIS2004-UTF8-V.bcmap")),
    ("UniJISPro-UCS2-HW-V", include_bytes!("../../../data/bcmaps/UniJISPro-UCS2-HW-V.bcmap")),
    ("UniJISPro-UCS2-V", include_bytes!("../../../data/bcmaps/UniJISPro-UCS2-V.bcmap")),
    ("UniJISPro-UTF8-V", include_bytes!("../../../data/bcmaps/UniJISPro-UTF8-V.bcmap")),
    ("UniJISX0213-UTF32-H", include_bytes!("../../../data/bcmaps/UniJISX0213-UTF32-H.bcmap")),
    ("UniJISX0213-UTF32-V", include_bytes!("../../../data/bcmaps/UniJISX0213-UTF32-V.bcmap")),
    ("UniJISX02132004-UTF32-H", include_bytes!("../../../data/bcmaps/UniJISX02132004-UTF32-H.bcmap")),
    ("UniJISX02132004-UTF32-V", include_bytes!("../../../data/bcmaps/UniJISX02132004-UTF32-V.bcmap")),
    ("V", include_bytes!("../../../data/bcmaps/V.bcmap")),
    ("WP-Symbol", include_bytes!("../../../data/bcmaps/WP-Symbol.bcmap")),
];

#[cfg(feature = "cmap-gb1")]
static BCMAPS_CMAP_GB1: &[(&str, &[u8])] = &[
    ("Adobe-GB1-0", include_bytes!("../../../data/bcmaps/Adobe-GB1-0.bcmap")),
    ("Adobe-GB1-1", include_bytes!("../../../data/bcmaps/Adobe-GB1-1.bcmap")),
    ("Adobe-GB1-2", include_bytes!("../../../data/bcmaps/Adobe-GB1-2.bcmap")),
    ("Adobe-GB1-3", include_bytes!("../../../data/bcmaps/Adobe-GB1-3.bcmap")),
    ("Adobe-GB1-4", include_bytes!("../../../data/bcmaps/Adobe-GB1-4.bcmap")),
    ("Adobe-GB1-5", include_bytes!("../../../data/bcmaps/Adobe-GB1-5.bcmap")),
    ("Adobe-GB1-UCS2", include_bytes!("../../../data/bcmaps/Adobe-GB1-UCS2.bcmap")),
    ("GB-EUC-H", include_bytes!("../../../data/bcmaps/GB-EUC-H.bcmap")),
    ("GB-EUC-V", include_bytes!("../../../data/bcmaps/GB-EUC-V.bcmap")),
    ("GB-H", include_bytes!("../../../data/bcmaps/GB-H.bcmap")),
    ("GB-V", include_bytes!("../../../data/bcmaps/GB-V.bcmap")),
    ("GBK-EUC-H", include_bytes!("../../../data/bcmaps/GBK-EUC-H.bcmap")),
    ("GBK-EUC-V", include_bytes!("../../../data/bcmaps/GBK-EUC-V.bcmap")),
    ("GBK2K-H", include_bytes!("../../../data/bcmaps/GBK2K-H.bcmap")),
    ("GBK2K-V", include_bytes!("../../../data/bcmaps/GBK2K-V.bcmap")),
    ("GBKp-EUC-H", include_bytes!("../../../data/bcmaps/GBKp-EUC-H.bcmap")),
    ("GBKp-EUC-V", include_bytes!("../../../data/bcmaps/GBKp-EUC-V.bcmap")),
    ("GBT-EUC-H", include_bytes!("../../../data/bcmaps/GBT-EUC-H.bcmap")),
    ("GBT-EUC-V", include_bytes!("../../../data/bcmaps/GBT-EUC-V.bcmap")),
    ("GBT-H", include_bytes!("../../../data/bcmaps/GBT-H.bcmap")),
    ("GBT-V", include_bytes!("../../../data/bcmaps/GBT-V.bcmap")),
    ("GBTpc-EUC-H", include_bytes!("../../../data/bcmaps/GBTpc-EUC-H.bcmap")),
    ("GBTpc-EUC-V", include_bytes!("../../../data/bcmaps/GBTpc-EUC-V.bcmap")),
    ("GBpc-EUC-H", include_bytes!("../../../data/bcmaps/GBpc-EUC-H.bcmap")),
    ("GBpc-EUC-V", include_bytes!("../../../data/bcmaps/GBpc-EUC-V.bcmap")),
    ("UniGB-UCS2-H", include_bytes!("../../../data/bcmaps/UniGB-UCS2-H.bcmap")),
    ("UniGB-UCS2-V", include_bytes!("../../../data/bcmaps/UniGB-UCS2-V.bcmap")),
    ("UniGB-UTF16-H", include_bytes!("../../../data/bcmaps/UniGB-UTF16-H.bcmap")),
    ("UniGB-UTF16-V", include_bytes!("../../../data/bcmaps/UniGB-UTF16-V.bcmap")),
    ("UniGB-UTF32-H", include_bytes!("../../../data/bcmaps/UniGB-UTF32-H.bcmap")),
    ("UniGB-UTF32-V", include_bytes!("../../../data/bcmaps/UniGB-UTF32-V.bcmap")),
    ("UniGB-UTF8-H", include_bytes!("../../../data/bcmaps/UniGB-UTF8-H.bcmap")),
    ("UniGB-UTF8-V", include_bytes!("../../../data/bcmaps/UniGB-UTF8-V.bcmap")),
];

#[cfg(feature = "cmap-cns1")]
static BCMAPS_CMAP_CNS1: &[(&str, &[u8])] = &[
    ("Adobe-CNS1-0", include_bytes!("../../../data/bcmaps/Adobe-CNS1-0.bcmap")),
    ("Adobe-CNS1-1", include_bytes!("../../../data/bcmaps/Adobe-CNS1-1.bcmap")),
    ("Adobe-CNS1-2", include_bytes!("../../../data/bcmaps/Adobe-CNS1-2.bcmap")),
    ("Adobe-CNS1-3", include_bytes!("../../../data/bcmaps/Adobe-CNS1-3.bcmap")),
    ("Adobe-CNS1-4", include_bytes!("../../../data/bcmaps/Adobe-CNS1-4.bcmap")),
    ("Adobe-CNS1-5", include_bytes!("../../../data/bcmaps/Adobe-CNS1-5.bcmap")),
    ("Adobe-CNS1-6", include_bytes!("../../../data/bcmaps/Adobe-CNS1-6.bcmap")),
    ("Adobe-CNS1-UCS2", include_bytes!("../../../data/bcmaps/Adobe-CNS1-UCS2.bcmap")),
    ("B5-H", include_bytes!("../../../data/bcmaps/B5-H.bcmap")),
    ("B5-V", include_bytes!("../../../data/bcmaps/B5-V.bcmap")),
    ("B5pc-H", include_bytes!("../../../data/bcmaps/B5pc-H.bcmap")),
    ("B5pc-V", include_bytes!("../../../data/bcmaps/B5pc-V.bcmap")),
    ("CNS-EUC-H", include_bytes!("../../../data/bcmaps/CNS-EUC-H.bcmap")),
    ("CNS-EUC-V", include_bytes!("../../../data/bcmaps/CNS-EUC-V.bcmap")),
    ("CNS1-H", include_bytes!("../../../data/bcmaps/CNS1-H.bcmap")),
    ("CNS1-V", include_bytes!("../../../data/bcmaps/CNS1-V.bcmap")),
    ("CNS2-H", include_bytes!("../../../data/bcmaps/CNS2-H.bcmap")),
    ("CNS2-V", include_bytes!("../../../data/bcmaps/CNS2-V.bcmap")),
    ("ETHK-B5-H", include_bytes!("../../../data/bcmaps/ETHK-B5-H.bcmap")),
    ("ETHK-B5-V", include_bytes!("../../../data/bcmaps/ETHK-B5-V.bcmap")),
    ("ETen-B5-H", include_bytes!("../../../data/bcmaps/ETen-B5-H.bcmap")),
    ("ETen-B5-V", include_bytes!("../../../data/bcmaps/ETen-B5-V.bcmap")),
    ("ETenms-B5-H", include_bytes!("../../../data/bcmaps/ETenms-B5-H.bcmap")),
    ("ETenms-B5-V", include_bytes!("../../../data/bcmaps/ETenms-B5-V.bcmap")),
    ("HKdla-B5-H", include_bytes!("../../../data/bcmaps/HKdla-B5-H.bcmap")),
    ("HKdla-B5-V", include_bytes!("../../../data/bcmaps/HKdla-B5-V.bcmap")),
    ("HKdlb-B5-H", include_bytes!("../../../data/bcmaps/HKdlb-B5-H.bcmap")),
    ("HKdlb-B5-V", include_bytes!("../../../data/bcmaps/HKdlb-B5-V.bcmap")),
    ("HKgccs-B5-H", include_bytes!("../../../data/bcmaps/HKgccs-B5-H.bcmap")),
    ("HKgccs-B5-V", include_bytes!("../../../data/bcmaps/HKgccs-B5-V.bcmap")),
    ("HKm314-B5-H", include_bytes!("../../../data/bcmaps/HKm314-B5-H.bcmap")),
    ("HKm314-B5-V", include_bytes!("../../../data/bcmaps/HKm314-B5-V.bcmap")),
    ("HKm471-B5-H", include_bytes!("../../../data/bcmaps/HKm471-B5-H.bcmap")),
    ("HKm471-B5-V", include_bytes!("../../../data/bcmaps/HKm471-B5-V.bcmap")),
    ("HKscs-B5-H", include_bytes!("../../../data/bcmaps/HKscs-B5-H.bcmap")),
    ("HKscs-B5-V", include_bytes!("../../../data/bcmaps/HKscs-B5-V.bcmap")),
    ("UniCNS-UCS2-H", include_bytes!("../../../data/bcmaps/UniCNS-UCS2-H.bcmap")),
    ("UniCNS-UCS2-V", include_bytes!("../../../data/bcmaps/UniCNS-UCS2-V.bcmap")),
    ("UniCNS-UTF16-H", include_bytes!("../../../data/bcmaps/UniCNS-UTF16-H.bcmap")),
    ("UniCNS-UTF16-V", include_bytes!("../../../data/bcmaps/UniCNS-UTF16-V.bcmap")),
    ("UniCNS-UTF32-H", include_bytes!("../../../data/bcmaps/UniCNS-UTF32-H.bcmap")),
    ("UniCNS-UTF32-V", include_bytes!("../../../data/bcmaps/UniCNS-UTF32-V.bcmap")),
    ("UniCNS-UTF8-H", include_bytes!("../../../data/bcmaps/UniCNS-UTF8-H.bcmap")),
    ("UniCNS-UTF8-V", include_bytes!("../../../data/bcmaps/UniCNS-UTF8-V.bcmap")),
];

#[cfg(feature = "cmap-korea1")]
static BCMAPS_CMAP_KOREA1: &[(&str, &[u8])] = &[
    ("Adobe-Korea1-0", include_bytes!("../../../data/bcmaps/Adobe-Korea1-0.bcmap")),
    ("Adobe-Korea1-1", include_bytes!("../../../data/bcmaps/Adobe-Korea1-1.bcmap")),
    ("Adobe-Korea1-2", include_bytes!("../../../data/bcmaps/Adobe-Korea1-2.bcmap")),
    ("Adobe-Korea1-UCS2", include_bytes!("../../../data/bcmaps/Adobe-Korea1-UCS2.bcmap")),
    ("KSC-EUC-H", include_bytes!("../../../data/bcmaps/KSC-EUC-H.bcmap")),
    ("KSC-EUC-V", include_bytes!("../../../data/bcmaps/KSC-EUC-V.bcmap")),
    ("KSC-H", include_bytes!("../../../data/bcmaps/KSC-H.bcmap")),
    ("KSC-Johab-H", include_bytes!("../../../data/bcmaps/KSC-Johab-H.bcmap")),
    ("KSC-Johab-V", include_bytes!("../../../data/bcmaps/KSC-Johab-V.bcmap")),
    ("KSC-V", include_bytes!("../../../data/bcmaps/KSC-V.bcmap")),
    ("KSCms-UHC-H", include_bytes!("../../../data/bcmaps/KSCms-UHC-H.bcmap")),
    ("KSCms-UHC-HW-H", include_bytes!("../../../data/bcmaps/KSCms-UHC-HW-H.bcmap")),
    ("KSCms-UHC-HW-V", include_bytes!("../../../data/bcmaps/KSCms-UHC-HW-V.bcmap")),
    ("KSCms-UHC-V", include_bytes!("../../../data/bcmaps/KSCms-UHC-V.bcmap")),
    ("KSCpc-EUC-H", include_bytes!("../../../data/bcmaps/KSCpc-EUC-H.bcmap")),
    ("KSCpc-EUC-V", include_bytes!("../../../data/bcmaps/KSCpc-EUC-V.bcmap")),
    ("UniKS-UCS2-H", include_bytes!("../../../data/bcmaps/UniKS-UCS2-H.bcmap")),
    ("UniKS-UCS2-V", include_bytes!("../../../data/bcmaps/UniKS-UCS2-V.bcmap")),
    ("UniKS-UTF16-H", include_bytes!("../../../data/bcmaps/UniKS-UTF16-H.bcmap")),
    ("UniKS-UTF16-V", include_bytes!("../../../data/bcmaps/UniKS-UTF16-V.bcmap")),
    ("UniKS-UTF32-H", include_bytes!("../../../data/bcmaps/UniKS-UTF32-H.bcmap")),
    ("UniKS-UTF32-V", include_bytes!("../../../data/bcmaps/UniKS-UTF32-V.bcmap")),
    ("UniKS-UTF8-H", include_bytes!("../../../data/bcmaps/UniKS-UTF8-H.bcmap")),
    ("UniKS-UTF8-V", include_bytes!("../../../data/bcmaps/UniKS-UTF8-V.bcmap")),
];


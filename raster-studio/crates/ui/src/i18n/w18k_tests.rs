//! W18-K: the gates for the languages wave 18 added to Photopea's More >
//! Language list — every table's every row draws with the faces
//! [`super::install_fonts`] installs, each new language is offered under its
//! own name, and no table carries right-to-left text the layout cannot show.

use super::{catalogue, install_fonts, tr_en, with_locale, Locale};

/// The languages wave 18 added, with the code the preferences file stores
/// and the name the language list shows (Photopea's own names, except that
/// Traditional Chinese is spelt in Traditional characters).
const W18_LANGUAGES: &[(Locale, &str, &str)] = &[
    (Locale::Nl, "nl", "Nederlands"),
    (Locale::Sv, "sv", "Svenska"),
    (Locale::Da, "da", "Dansk"),
    (Locale::No, "no", "Norsk"),
    (Locale::Fi, "fi", "Suomi"),
    (Locale::Cs, "cs", "\u{10c}esky"),
    (Locale::Sk, "sk", "Sloven\u{10d}ina"),
    (Locale::Hu, "hu", "Magyar"),
    (Locale::Ro, "ro", "Rom\u{e2}n\u{103}"),
    (Locale::Pt, "pt", "Portugu\u{ea}s"),
    (Locale::Ca, "ca", "Catal\u{e0}"),
    (Locale::Hr, "hr", "Hrvatski"),
    (Locale::Sl, "sl", "Sloven\u{161}\u{10d}ina"),
    (Locale::Id, "id", "Bahasa Indonesia"),
    (Locale::Vi, "vi", "Ti\u{1ebf}ng Vi\u{1ec7}t"),
    (Locale::ZhTw, "zh-TW", "\u{7e41}\u{9ad4}\u{4e2d}\u{6587}"),
    (
        Locale::El,
        "el",
        "\u{395}\u{3bb}\u{3bb}\u{3b7}\u{3bd}\u{3b9}\u{3ba}\u{3ac}",
    ),
    (
        Locale::Bg,
        "bg",
        "\u{411}\u{44a}\u{43b}\u{433}\u{430}\u{440}\u{441}\u{43a}\u{438} \u{435}\u{437}\u{438}\u{43a}",
    ),
    (
        Locale::Sr,
        "sr",
        "\u{421}\u{440}\u{43f}\u{441}\u{43a}\u{438} \u{458}\u{435}\u{437}\u{438}\u{43a}",
    ),
    (
        Locale::Mk,
        "mk",
        "\u{41c}\u{430}\u{43a}\u{435}\u{434}\u{43e}\u{43d}\u{441}\u{43a}\u{438}",
    ),
    (Locale::Et, "et", "Eesti"),
    (Locale::Lt, "lt", "Lietuvi\u{173}"),
    (Locale::Eo, "eo", "Esperanto"),
    (Locale::Sq, "sq", "Shqip"),
    (Locale::Tl, "tl", "Tagalog"),
    (Locale::Kk, "kk", "\u{49a}\u{430}\u{437}\u{430}\u{49b}\u{448}\u{430}"),
];

fn ready(ctx: &egui::Context) {
    let _ = ctx.run(egui::RawInput::default(), |_| {});
}

fn fonts() -> (egui::FontId, egui::FontId) {
    let body = design::egui_theme::font_id(design::Theme::Dark.tokens(), design::TypeRole::Body);
    let mono = egui::FontId {
        family: egui::FontFamily::Monospace,
        ..body.clone()
    };
    (body, mono)
}

/// Every non-English table's name and every row draws in both font families
/// once the bundled faces are installed: no tofu anywhere in any language.
#[test]
fn every_offered_language_draws_every_row_with_the_installed_faces() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    ready(&ctx);
    let (body, mono) = fonts();
    let mut problems = Vec::new();
    for locale in Locale::ALL.iter().copied().filter(|l| *l != Locale::En) {
        let table = catalogue(locale).expect("every non-English locale has a table");
        for text in std::iter::once(&table.name).chain(table.rows.values()) {
            let drawn = ctx.fonts(|f| f.has_glyphs(&body, text) && f.has_glyphs(&mono, text));
            if !drawn {
                let missing: String = text
                    .chars()
                    .filter(|c| !ctx.fonts(|f| f.has_glyphs(&body, &c.to_string())))
                    .collect();
                problems.push(format!("{locale:?}: {text:?} lacks {missing:?}"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    // The check can fail: a Thai letter has no face in the chain.
    assert!(!ctx.fonts(|f| f.has_glyphs(&body, "\u{e20}")));
}

/// The Vietnamese table needs the bundled Vietnamese face: egui's own fonts
/// cannot draw its precomposed letters, the installed chain can.
#[test]
fn the_vietnamese_rows_need_the_bundled_vietnamese_face() {
    let (body, _) = fonts();
    let bare = egui::Context::default();
    ready(&bare);
    let name = Locale::Vi.display_name();
    assert!(
        !bare.fonts(|f| f.has_glyphs(&body, name)),
        "egui alone already draws {name:?}; the face would prove nothing"
    );
    let table = catalogue(Locale::Vi).expect("a Vietnamese table");
    let needs_face = table
        .rows
        .values()
        .filter(|t| t.chars().any(|c| ('\u{1ea0}'..='\u{1ef9}').contains(&c)))
        .count();
    assert!(
        needs_face > 500,
        "only {needs_face} rows use the extended letters"
    );
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    ready(&ctx);
    assert!(ctx.fonts(|f| f.has_glyphs(&body, name)));
    assert!(Locale::Vi.needs_vietnamese_font());
    assert!(!Locale::Pt.needs_vietnamese_font());
}

/// Traditional Chinese is its own table in Traditional characters, drawn by
/// the CJK face, not the Simplified table under another name.
#[test]
fn traditional_chinese_is_its_own_table_and_the_cjk_face_draws_it() {
    assert!(Locale::ZhTw.needs_cjk_font());
    let file = |l| with_locale(l, || tr_en("File").to_string());
    assert_eq!(file(Locale::ZhTw), "\u{6a94}\u{6848}");
    assert_eq!(file(Locale::ZhCn), "\u{6587}\u{4ef6}");
    let (body, _) = fonts();
    let bare = egui::Context::default();
    ready(&bare);
    assert!(!bare.fonts(|f| f.has_glyphs(&body, "\u{6a94}\u{6848}")));
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    ready(&ctx);
    let table = catalogue(Locale::ZhTw).expect("a Traditional Chinese table");
    for text in table.rows.values() {
        assert!(ctx.fonts(|f| f.has_glyphs(&body, text)), "{text:?}");
    }
}

/// Each wave-18 language is offered: its code round-trips, its name is its
/// own, and the menu bar model speaks it rather than English.
#[test]
fn every_w18_language_is_offered_under_its_own_name_and_translates_the_bar() {
    let english: Vec<&str> = crate::menu::menu_bar(0).iter().map(|m| m.title).collect();
    for (locale, code, name) in W18_LANGUAGES {
        assert!(Locale::ALL.contains(locale), "{locale:?} is not offered");
        assert_eq!(locale.code(), *code);
        assert_eq!(Locale::from_code(code), *locale);
        assert_eq!(locale.display_name(), *name);
        let titles: Vec<&str> = with_locale(*locale, || {
            crate::menu::menu_bar(0).iter().map(|m| m.title).collect()
        });
        assert_eq!(titles.len(), english.len());
        let changed = titles.iter().zip(&english).filter(|(t, e)| t != e).count();
        // "Filter" and "Help" are the same word in some languages.
        assert!(
            changed >= english.len() - 2,
            "{locale:?} leaves the bar in English: {titles:?}"
        );
    }
    // English, the twelve wave-16 languages and these twenty-six.
    assert_eq!(Locale::ALL.len(), 39, "{} languages", Locale::ALL.len());
    assert_eq!(W18_LANGUAGES.len(), 26);
}

/// No table carries Hebrew, Arabic or other right-to-left text: the layout
/// draws every line left to right, so such a row would read backwards.
#[test]
fn no_table_carries_right_to_left_text() {
    let rtl = |c: char| {
        ('\u{590}'..='\u{8ff}').contains(&c)
            || ('\u{fb1d}'..='\u{fdff}').contains(&c)
            || ('\u{fe70}'..='\u{feff}').contains(&c)
    };
    assert!(rtl('\u{5d0}'), "the check recognises Hebrew");
    for locale in Locale::ALL.iter().copied().filter(|l| *l != Locale::En) {
        let table = catalogue(locale).expect("a table");
        for text in std::iter::once(&table.name).chain(table.rows.values()) {
            assert!(!text.chars().any(rtl), "{locale:?}: {text:?}");
        }
    }
}

/// The menu bar speaks Photopea's own words: each wave-18 table takes the
/// menu titles from the matching table in Photopea's bundle (pp/file.json),
/// except where that table's entry is not a word of the language (the
/// Bulgarian "Edit" entry there reads "ya", so the table keeps its own).
const PHOTOPEA_TITLES: &[(Locale, &[(&str, &str)])] = &[
    (
        Locale::Nl,
        &[
            ("File", "Bestand"),
            ("Edit", "Bewerken"),
            ("Image", "Afbeelding"),
            ("Layer", "Laag"),
            ("Select", "Selecteren"),
            ("Filter", "Filter"),
            ("View", "Weergave"),
            ("Window", "Venster"),
            ("Help", "Help"),
        ],
    ),
    (
        Locale::Sv,
        &[
            ("File", "Fil"),
            ("Edit", "Redigera"),
            ("Image", "Bild"),
            ("Layer", "Lager"),
            ("Select", "Markera"),
            ("Filter", "Filter"),
            ("View", "Vy"),
            ("Window", "F\u{f6}nster"),
            ("Help", "Hj\u{e4}lp"),
        ],
    ),
    (
        Locale::Da,
        &[
            ("File", "Fil"),
            ("Edit", "Rediger"),
            ("Image", "Billede"),
            ("Layer", "Lag"),
            ("Select", "Marker"),
            ("Filter", "Filter"),
            ("View", "Vis"),
            ("Window", "Vindue"),
            ("Help", "Hj\u{e6}lp"),
        ],
    ),
    (
        Locale::No,
        &[
            ("File", "Fil"),
            ("Edit", "Rediger"),
            ("Image", "Bilde"),
            ("Layer", "Lag"),
            ("Select", "Velg"),
            ("Filter", "Filter"),
            ("View", "Visning"),
            ("Window", "Vindu"),
            ("Help", "Hjelp"),
        ],
    ),
    (
        Locale::Fi,
        &[
            ("File", "Tiedosto"),
            ("Edit", "Muokkaa"),
            ("Image", "Kuva"),
            ("Layer", "Taso"),
            ("Select", "Valitse"),
            ("Filter", "Suodatin"),
            ("View", "N\u{e4}kym\u{e4}"),
            ("Window", "Ikkuna"),
            ("Help", "Ohje"),
        ],
    ),
    (
        Locale::Cs,
        &[
            ("File", "Soubor"),
            ("Edit", "\u{da}pravy"),
            ("Image", "Obraz"),
            ("Layer", "Vrstva"),
            ("Select", "V\u{fd}b\u{11b}r"),
            ("Filter", "Filtr"),
            ("View", "Zobrazit"),
            ("Window", "Okno"),
            ("Help", "N\u{e1}pov\u{11b}da"),
        ],
    ),
    (
        Locale::Sk,
        &[
            ("File", "S\u{fa}bor"),
            ("Edit", "Upravi\u{165}"),
            ("Image", "Obraz"),
            ("Layer", "Vrstva"),
            ("Select", "Vybra\u{165}"),
            ("Filter", "Filter"),
            ("View", "Zobrazenie"),
            ("Window", "Okno"),
            ("Help", "Pomoc"),
        ],
    ),
    (
        Locale::Hu,
        &[
            ("File", "F\u{e1}jl"),
            ("Edit", "Szerkeszt\u{e9}s"),
            ("Image", "K\u{e9}p"),
            ("Layer", "R\u{e9}teg"),
            ("Select", "Kijel\u{f6}l\u{e9}s"),
            ("Filter", "Sz\u{171}r\u{151}"),
            ("View", "N\u{e9}zet"),
            ("Window", "Ablak"),
            ("Help", "S\u{fa}g\u{f3}"),
        ],
    ),
    (
        Locale::Ro,
        &[
            ("File", "Fi\u{219}ier"),
            ("Edit", "Editare"),
            ("Image", "Imagine"),
            ("Layer", "Strat"),
            ("Select", "Selecteaz\u{103}"),
            ("Filter", "Filtru"),
            ("View", "Vizualizare"),
            ("Window", "Fereastr\u{103}"),
            ("Help", "Ajutor"),
        ],
    ),
    (
        Locale::Pt,
        &[
            ("File", "Arquivo"),
            ("Edit", "Editar"),
            ("Image", "Imagem"),
            ("Layer", "Camada"),
            ("Select", "Selecionar"),
            ("Filter", "Filtro"),
            ("View", "Visualizar"),
            ("Window", "Janela"),
            ("Help", "Ajuda"),
        ],
    ),
    (
        Locale::Ca,
        &[
            ("File", "Fitxer"),
            ("Edit", "Edita"),
            ("Image", "Imatge"),
            ("Layer", "Capa"),
            ("Select", "Selecciona"),
            ("Filter", "Filtre"),
            ("View", "Visualitza"),
            ("Window", "Finestra"),
            ("Help", "Ajuda"),
        ],
    ),
    (
        Locale::Hr,
        &[
            ("File", "Datoteka"),
            ("Edit", "Ure\u{111}ivanje"),
            ("Image", "Slika"),
            ("Layer", "Sloj"),
            ("Select", "Odabir"),
            ("Filter", "Filter"),
            ("View", "Prikaz"),
            ("Window", "Prozor"),
            ("Help", "Upomo\u{107}"),
        ],
    ),
    (
        Locale::Sl,
        &[
            ("File", "Datoteka"),
            ("Edit", "Uredi"),
            ("Image", "Slika"),
            ("Layer", "Plast"),
            ("Select", "Izberi"),
            ("Filter", "Sito"),
            ("View", "Ogled"),
            ("Window", "Okno"),
            ("Help", "Pomo\u{10d}"),
        ],
    ),
    (
        Locale::Id,
        &[
            ("File", "Berkas"),
            ("Edit", "Ubah"),
            ("Image", "Gambar"),
            ("Layer", "Lapisan"),
            ("Select", "Pilih"),
            ("Filter", "Filter"),
            ("View", "Tampilan"),
            ("Window", "Jendela"),
            ("Help", "Bantuan"),
        ],
    ),
    (
        Locale::Vi,
        &[
            ("File", "T\u{1ec7}p"),
            ("Edit", "Ch\u{1ec9}nh s\u{1eed}a"),
            ("Image", "H\u{ec}nh \u{1ea3}nh"),
            ("Layer", "Layer"),
            ("Select", "L\u{1ef1}a ch\u{1ecd}n"),
            ("Filter", "B\u{1ed9} l\u{1ecd}c"),
            ("View", "Xem"),
            ("Window", "C\u{1eed}a s\u{1ed5}"),
            ("Help", "Tr\u{1ee3} gi\u{fa}p"),
        ],
    ),
    (
        Locale::ZhTw,
        &[
            ("File", "\u{6a94}\u{6848}"),
            ("Edit", "\u{7de8}\u{8f2f}"),
            ("Image", "\u{5f71}\u{50cf}"),
            ("Layer", "\u{5716}\u{5c64}"),
            ("Select", "\u{9078}\u{53d6}"),
            ("Filter", "\u{6ffe}\u{93e1}"),
            ("View", "\u{6aa2}\u{8996}"),
            ("Window", "\u{8996}\u{7a97}"),
            ("Help", "\u{5e6b}\u{52a9}"),
        ],
    ),
    (
        Locale::El,
        &[
            ("File", "\u{391}\u{3c1}\u{3c7}\u{3b5}\u{3af}\u{3bf}"),
            (
                "Edit",
                "\u{395}\u{3c0}\u{3b5}\u{3be}\u{3b5}\u{3c1}\u{3b3}\u{3b1}\u{3c3}\u{3af}\u{3b1}",
            ),
            ("Image", "\u{395}\u{3b9}\u{3ba}\u{3cc}\u{3bd}\u{3b1}"),
            ("Layer", "\u{395}\u{3c0}\u{3af}\u{3c0}\u{3b5}\u{3b4}\u{3bf}"),
            (
                "Select",
                "\u{395}\u{3c0}\u{3b9}\u{3bb}\u{3bf}\u{3b3}\u{3ae}",
            ),
            ("Filter", "\u{3a6}\u{3af}\u{3bb}\u{3c4}\u{3c1}\u{3b1}"),
            ("View", "\u{3a0}\u{3c1}\u{3bf}\u{3b2}\u{3bf}\u{3bb}\u{3ae}"),
            (
                "Window",
                "\u{3a0}\u{3b1}\u{3c1}\u{3ac}\u{3b8}\u{3c5}\u{3c1}\u{3bf}",
            ),
            ("Help", "\u{392}\u{3bf}\u{3ae}\u{3b8}\u{3b5}\u{3b9}\u{3b1}"),
        ],
    ),
    (
        Locale::Bg,
        &[
            ("File", "\u{424}\u{430}\u{439}\u{43b}"),
            (
                "Image",
                "\u{418}\u{437}\u{43e}\u{431}\u{440}\u{430}\u{436}\u{435}\u{43d}\u{438}\u{435}",
            ),
            ("Layer", "\u{421}\u{43b}\u{43e}\u{439}"),
            (
                "Select",
                "\u{418}\u{437}\u{431}\u{435}\u{440}\u{435}\u{442}\u{435}",
            ),
            ("Filter", "\u{424}\u{438}\u{43b}\u{442}\u{44a}\u{440}"),
            ("View", "\u{418}\u{437}\u{433}\u{43b}\u{435}\u{434}"),
            (
                "Window",
                "\u{41f}\u{440}\u{43e}\u{437}\u{43e}\u{440}\u{435}\u{446}",
            ),
            ("Help", "\u{41f}\u{43e}\u{43c}\u{43e}\u{449}"),
        ],
    ),
    (
        Locale::Sr,
        &[
            (
                "File",
                "\u{414}\u{430}\u{442}\u{43e}\u{442}\u{435}\u{43a}\u{430}",
            ),
            ("Edit", "\u{418}\u{437}\u{43c}\u{435}\u{43d}\u{438}"),
            ("Image", "\u{421}\u{43b}\u{438}\u{43a}\u{430}"),
            ("Layer", "\u{421}\u{43b}\u{43e}\u{458}"),
            (
                "Select",
                "\u{418}\u{437}\u{430}\u{431}\u{435}\u{440}\u{438}",
            ),
            ("Filter", "\u{424}\u{438}\u{43b}\u{442}\u{430}\u{440}"),
            ("View", "\u{41f}\u{43e}\u{433}\u{43b}\u{435}\u{434}"),
            ("Window", "\u{41f}\u{440}\u{43e}\u{437}\u{43e}\u{440}"),
            ("Help", "\u{41f}\u{43e}\u{43c}\u{43e}\u{45b}"),
        ],
    ),
    (
        Locale::Mk,
        &[
            (
                "File",
                "\u{414}\u{430}\u{442}\u{43e}\u{442}\u{435}\u{43a}\u{430}",
            ),
            ("Edit", "\u{423}\u{440}\u{435}\u{434}\u{438}"),
            (
                "Image",
                "\u{424}\u{43e}\u{442}\u{43e}\u{433}\u{440}\u{430}\u{444}\u{438}\u{458}\u{430}",
            ),
            ("Layer", "\u{421}\u{43b}\u{43e}\u{458}"),
            (
                "Select",
                "\u{418}\u{437}\u{431}\u{435}\u{440}\u{435}\u{442}\u{435}",
            ),
            ("Filter", "\u{424}\u{438}\u{43b}\u{442}\u{435}\u{440}"),
            ("View", "\u{41f}\u{43e}\u{433}\u{43b}\u{435}\u{434}"),
            (
                "Window",
                "\u{41f}\u{440}\u{43e}\u{437}\u{43e}\u{440}\u{435}\u{446}",
            ),
            ("Help", "\u{41f}\u{43e}\u{43c}\u{43e}\u{448}"),
        ],
    ),
    (
        Locale::Et,
        &[
            ("File", "Fail"),
            ("Edit", "Muuda"),
            ("Image", "Pilt"),
            ("Layer", "Kiht"),
            ("Select", "M\u{e4}rgista"),
            ("Filter", "Filter"),
            ("View", "Vaade"),
            ("Window", "Aken"),
            ("Help", "Abi"),
        ],
    ),
    (
        Locale::Lt,
        &[
            ("File", "Failas"),
            ("Edit", "Redaguoti"),
            ("Image", "Vaizdas"),
            ("Layer", "Sluoksnis"),
            ("Select", "\u{17d}ym\u{117}ti"),
            ("Filter", "Filtrai"),
            ("View", "Rodymas"),
            ("Window", "Langai"),
            ("Help", "\u{17d}inynas"),
        ],
    ),
    (
        Locale::Eo,
        &[
            ("File", "Dosiero"),
            ("Edit", "Redakti"),
            ("Image", "Bildo"),
            ("Layer", "Tavolo"),
            ("Select", "Elekti"),
            ("Filter", "Filtrilo"),
            ("View", "Vido"),
            ("Window", "Fenestro"),
            ("Help", "Helpo"),
        ],
    ),
    (
        Locale::Sq,
        &[
            ("File", "Dokumenti"),
            ("Edit", "Redakto"),
            ("Image", "Foto"),
            ("Layer", "Shtresa"),
            ("Select", "P\u{eb}rzgjidh"),
            ("Filter", "Filtrues"),
            ("View", "Pamja"),
            ("Window", "Dritarja"),
            ("Help", "Ndihme"),
        ],
    ),
    (
        Locale::Tl,
        &[
            ("File", "File"),
            ("Edit", "I-edit"),
            ("Image", "Larawan"),
            ("Layer", "Layer"),
            ("Select", "Piliin"),
            ("Filter", "Salain"),
            ("View", "Pagtingin"),
            ("Window", "Bintana"),
            ("Help", "Tulong"),
        ],
    ),
    (
        Locale::Kk,
        &[
            ("File", "\u{424}\u{430}\u{439}\u{43b}"),
            ("Edit", "\u{4e8}\u{4a3}\u{434}\u{435}\u{443}"),
            ("Image", "\u{421}\u{443}\u{440}\u{435}\u{442}"),
            ("Layer", "\u{49a}\u{430}\u{431}\u{430}\u{442}"),
            ("Select", "\u{422}\u{430}\u{4a3}\u{434}\u{430}\u{443}"),
            ("Filter", "\u{421}\u{4af}\u{437}\u{433}\u{456}"),
            ("View", "\u{41a}\u{4e9}\u{440}\u{443}"),
            ("Window", "\u{422}\u{435}\u{440}\u{435}\u{437}\u{435}"),
            ("Help", "\u{41a}\u{4e9}\u{43c}\u{435}\u{43a}"),
        ],
    ),
];

#[test]
fn the_menu_titles_are_photopeas_own_words_for_each_w18_language() {
    assert_eq!(PHOTOPEA_TITLES.len(), W18_LANGUAGES.len());
    for (locale, titles) in PHOTOPEA_TITLES {
        for (english, photopea) in *titles {
            let ours = with_locale(*locale, || tr_en(english).to_string());
            assert_eq!(&ours, photopea, "{locale:?}: {english:?}");
        }
        let drawn: Vec<&str> = with_locale(*locale, || {
            crate::menu::menu_bar(0).iter().map(|m| m.title).collect()
        });
        for (_, photopea) in *titles {
            assert!(
                drawn.contains(photopea),
                "{locale:?}: {photopea:?} not in {drawn:?}"
            );
        }
    }
    let bg_edit = with_locale(Locale::Bg, || tr_en("Edit").to_string());
    assert_ne!(bg_edit, "ya");
}

/// W18-K: the Kazakh table writes the Cyrillic letters Kazakh adds to the
/// Russian alphabet (the glyph gate above then draws every row of it), and
/// egui's own faces, with no bundled face installed, draw all eighteen of
/// those letters in both families: Kazakh needs no extra font.
#[test]
fn the_kazakh_table_uses_the_kazakh_letters_and_needs_no_bundled_face() {
    let table = catalogue(Locale::Kk).expect("a Kazakh table");
    let text: String = table.rows.values().copied().collect();
    for letter in "\u{4d9}\u{493}\u{49b}\u{4a3}\u{4e9}\u{4b1}\u{4af}\u{456}\u{49a}\u{4e8}\u{4b0}\u{4ae}\u{406}\u{4d8}".chars() {
        assert!(text.contains(letter), "no {letter:?} in the Kazakh table");
    }
    let ctx = egui::Context::default();
    ready(&ctx);
    let (body, mono) = fonts();
    let letters = "\u{4d8}\u{4d9}\u{492}\u{493}\u{49a}\u{49b}\u{4a2}\u{4a3}\u{4e8}\u{4e9}\u{4b0}\u{4b1}\u{4ae}\u{4af}\u{4ba}\u{4bb}\u{406}\u{456}";
    assert_eq!(letters.chars().count(), 18);
    for letter in letters.chars().map(String::from) {
        assert!(
            ctx.fonts(|f| f.has_glyphs(&body, &letter) && f.has_glyphs(&mono, &letter)),
            "egui's own faces lack {letter:?}"
        );
    }
    assert!(!Locale::Kk.needs_cjk_font());
    assert!(!Locale::Kk.needs_vietnamese_font());
    assert_eq!(
        with_locale(Locale::Kk, || tr_en("Layers").to_string()),
        "\u{49a}\u{430}\u{431}\u{430}\u{442}\u{442}\u{430}\u{440}"
    );
}

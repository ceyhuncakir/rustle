// Keyboard layouts that have no 'v' key, for the paste chord.
//
// Mutter's virtual keyboard can only press a keyval the *current* layout
// has, so on a Russian or Greek layout notify_keyval(KEY_v) sends nothing at
// all and Mutter logs "No keycode found for keyval". There the chord goes out
// as evdev keycodes instead: Ctrl plus the physical V key, which toolkits
// resolve to Ctrl+V through the Latin layout the user switches to. Latin
// layouts keep using keyvals, so Dvorak, Colemak and the rest press the key
// that actually says V.
//
// Generated from xkeyboard-config 2.44 with libxkbcommon: every layout and
// variant whose first level has no 'v' keysym (a 'v' only reachable with
// AltGr counts as none; the chord would come out as Ctrl+AltGr). Ids are
// 'layout' or 'layout+variant', as in org.gnome.desktop.input-sources.

// Layouts without a 'v'...
const NON_LATIN_LAYOUTS = new Set([
    'af', 'am', 'ara', 'bd', 'bg', 'brai', 'bt', 'by', 'eg', 'et', 'ge', 'gn',
    'gr', 'il', 'in', 'iq', 'ir', 'kg', 'kh', 'kz', 'la', 'lk', 'ma', 'mk',
    'mm', 'mn', 'mv', 'my', 'np', 'pk', 'rs', 'ru', 'sy', 'th', 'tj', 'tm',
    'tz', 'ua', 'uz',
]);

// ...except these variants of them, which are Latin.
const LATIN_VARIANTS = new Set([
    'by+latin', 'in+eng', 'iq+ku', 'iq+ku_alt', 'iq+ku_f', 'ir+ku',
    'ir+ku_alt', 'ir+ku_f', 'kz+latin', 'lk+us', 'ma+french', 'ma+rif',
    'rs+latin', 'rs+latinalternatequotes', 'rs+latinunicode',
    'rs+latinunicodeyz', 'rs+latinyz', 'ru+cv_latin', 'ru+ruchey_en', 'sy+ku',
    'sy+ku_alt', 'sy+ku_f', 'tm+alt', 'ua+crh', 'ua+crh_alt', 'ua+crh_f',
    'uz+latin',
]);

// Variants of Latin layouts that are not.
const NON_LATIN_VARIANTS = new Set([
    'al+veqilharxhi', 'az+cyrillic', 'br+rus', 'ca+ike',
    'cn+mon_manchu_galik', 'cn+mon_todo_galik', 'cn+mon_trad',
    'cn+mon_trad_galik', 'cn+mon_trad_manchu', 'cn+mon_trad_todo',
    'cn+mon_trad_xibe', 'cn+tib', 'cn+tib_asciinum', 'cn+ug', 'cz+rus',
    'cz+ucw', 'de+ru', 'dz+ar', 'dz+ber', 'fr+geo', 'id+javanese',
    'id+melayu-phonetic', 'id+melayu-phoneticx', 'id+pegon-phonetic',
    'ie+ogam', 'it+geo', 'jp+kana', 'jp+mac', 'lv+modern-cyr', 'me+cyrillic',
    'me+cyrillicalternatequotes', 'me+cyrillicyz', 'ng+yoruba',
    'ph+capewell-dvorak-bay', 'ph+capewell-qwerf2k6-bay', 'ph+colemak-bay',
    'ph+dvorak-bay', 'ph+qwerty-bay', 'pl+ru_phonetic_dvorak', 'se+rus',
    'se+swl', 'us+chr', 'us+rus',
]);

/**
 * Whether the xkb layout `xkbId` ('ru', 'us+dvorak', ...) lacks a 'v' key.
 * Unknown ids count as Latin.
 */
export function isNonLatinLayout(xkbId) {
    if (!xkbId)
        return false;

    const layout = xkbId.split('+')[0];
    if (NON_LATIN_LAYOUTS.has(layout))
        return !LATIN_VARIANTS.has(xkbId);
    return NON_LATIN_VARIANTS.has(xkbId);
}

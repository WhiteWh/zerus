# Optional desktop translations

The desktop starts in English regardless of operating-system language. Settings >
Appearance > Interface language offers **Russian (preview)**. The preference is
local to this desktop (`workspace/language`); changing it takes effect after
restarting Zerus. It does not restart native agents, alter accounts or translate
agent messages, command output, identities or filenames. English remains available
as an explicit choice. An old `system` value or unsupported value means English.

This refresh adapts the catalog originally contributed by
[frolovnd in PR #6](https://github.com/ufna/zerus/pull/6). The source catalog now
contains the strings extracted from the current desktop rather than retired UI.
It is a **partial preview**, not a complete Russian translation: finished entries
retain the contributor's translations; new untranslated entries are explicitly
`type="unfinished"` and display their original English. Platform-native Qt
dialogs use Russian only if that Qt installation supplies `qtbase_ru`.

## Refresh and review

Install Qt's Linguist tools (`qt6-tools` on Arch). From the repository root:

```sh
python3 tray/translations/refresh_catalog.py
python3 -m unittest discover -s tray/translations -p 'test_*.py'
python3 tray/translations/prepare_catalog.py tray/translations/zerus_ru.ts /tmp/zerus_ru.ts
lrelease6 /tmp/zerus_ru.ts -nounfinished -qm /tmp/zerus_ru.qm
```

`refresh_catalog.py` runs the installed trusted `lupdate` over `tray/src`. It keeps
exact context/source/disambiguation/plural matches and reuses moved translations
only when every completed candidate agrees. Retired entries disappear; new ones
stay unfinished. Review the printed counts and the entire catalog diff. Translate
each unfinished entry, preserve placeholders and markup, then remove its
`unfinished` attribute. Do not mark an English placeholder as a finished Russian
translation to inflate coverage.

The build validates every finished entry before `lrelease -nounfinished`. Russian
plural entries require all three forms, and placeholder and markup sequences must
stay exact. A generated `Zerus` fallback context supports existing header-only
widgets whose `tr()` inherits a Qt base class context. It contains only completed,
unambiguous entries without disambiguation; it never guesses an agent response.
The compiled catalog is embedded in the app, not committed as a binary.

The Tauri client's `client/src/i18n.ru.json` is generated from this catalog by
`client/scripts/generate-i18n-catalog.py`; never edit the JSON by hand. A source
that reads differently in one use carries a Qt disambiguation, for example
`tr("Pause", "media playback")`; the generator keys it as
`"media playback\u0004Pause"` and the client reads it with `trc()`.

Strings that exist only in Next have no Qt source. Their reviewed Russian lives
in `client/src/i18n.next-ru.json`, which the generator merges under the same
rules: exact placeholders, three forms for a `%n` plural, no separator dot, and
never a source that `zerus_ru.ts` already translates (translate those here).
`client/src/i18n.english.json` lists the sources still shown in English; the
client's coverage test keeps both lists current.

Run the `ui-language` and `translation-catalog` CTest suites after building the
desktop. `test_ui_language` uses private INI preferences and tests the English
default under a Russian locale, explicit persistence, resource loading,
context fallback and plural counts. Its optional `ZERUS_LANGUAGE_PREVIEW` PNG is
a synthetic language-choice pane, not a qualification of the complete translated
desktop. New platform builds and translated layout review remain necessary before
claiming platform delivery or complete localization.

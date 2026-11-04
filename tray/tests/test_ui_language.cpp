#include <QtTest>
#include "Settings.h"
#include <QLocale>
#include <QLabel>
#include <QTemporaryDir>
#include <QVBoxLayout>
#include <memory>
#include "UiLanguage.h"

class UiLanguageTest : public QObject {
    Q_OBJECT
    QTemporaryDir settings;
    QLocale previousLocale;
    ZerusTranslator translator;
    QTranslator qtTranslator;
private slots:
    void initTestCase() {
        QVERIFY(settings.isValid());
        QCoreApplication::setOrganizationName("ZerusLanguageFixture");
        QCoreApplication::setApplicationName("ZerusLanguageFixture");
        QSettings::setDefaultFormat(QSettings::IniFormat);
        QSettings::setPath(QSettings::IniFormat, QSettings::UserScope, settings.path());
        QSettings::setPath(QSettings::IniFormat, QSettings::SystemScope, settings.path());
    }
    void init() { Settings::flush();QSettings().clear(); }
    void cleanup() {
        qApp->removeTranslator(&translator);
        qApp->removeTranslator(&qtTranslator);
        QLocale::setDefault(previousLocale);
    }
    void englishDefaultIgnoresSystemLocaleAndLegacyPreferences() {
        QLocale::setDefault(QLocale("ru_RU"));
        QCOMPARE(zerusUiLanguage(), QString("en"));
        QVERIFY(installZerusTranslation(*qApp, translator, qtTranslator));
        QCOMPARE(QCoreApplication::translate("SettingsPage", "Appearance"), QString("Appearance"));
        for (const auto &value : {"system", "unknown", "", "RU"}) {
            QSettings().setValue("workspace/language", value);
            QCOMPARE(zerusUiLanguage(), QString("en"));
        }
    }
    void explicitChoicePersistsOnlyLanguageAndNeedsRestart() {
        QSettings().setValue("fixture/unchanged", "keep");
        std::unique_ptr<QComboBox> choice(zerusLanguageChoice());
        QCOMPARE(choice->currentData().toString(), QString("en"));
        choice->setCurrentIndex(choice->findData("ru"));
        QCOMPARE(zerusUiLanguage(), QString("ru"));
        QCOMPARE(QCoreApplication::translate("SettingsPage", "Appearance"), QString("Appearance"));
        QCOMPARE(Settings::value("fixture/unchanged").toString(), QString("keep"));
        QCOMPARE(QSettings().allKeys(), QStringList({"fixture/unchanged", "workspace/language"}));
    }
    void russianResourceKeepsNativeAndUnfinishedText() {
        QSettings().setValue("workspace/language", "ru");
        QVERIFY(installZerusTranslation(*qApp, translator, qtTranslator));
        QCOMPARE(QCoreApplication::translate("SettingsPage", "Appearance"), QString::fromUtf8("Внешний вид"));
        // Widgets without Q_OBJECT use inherited tr(); only unambiguous aliases translate.
        QCOMPARE(QCoreApplication::translate("QWidget", "Settings"), QString::fromUtf8("Настройки"));
        QCOMPARE(translator.translate("Unknown", "Settings", "unreviewed disambiguation"), QString());
        QCOMPARE(QCoreApplication::translate("NativeTranscript", "Synthetic agent reply"), QString("Synthetic agent reply"));
        // An entry still unfinished in zerus_ru.ts stays English. The Next client
        // fills only strings it uses, so pick a Qt-only one ("Reset to %1%").
        QCOMPARE(QCoreApplication::translate("SettingsPage", "Reset to %1%"), QString("Reset to %1%"));
        QCOMPARE(QCoreApplication::translate("QObject", "Russian (preview)"), QString::fromUtf8("Русский (предпросмотр)"));
    }
    void pluralFormsRetainCounts() {
        QSettings().setValue("workspace/language", "ru");
        QVERIFY(installZerusTranslation(*qApp, translator, qtTranslator));
        const char *text = "Project sync connected to %n computer(s)";
        QCOMPARE(QCoreApplication::translate("SwarmController", text, nullptr, 1),
                 QString::fromUtf8("Синхронизация проектов: подключён 1 компьютер"));
        QCOMPARE(QCoreApplication::translate("SwarmController", text, nullptr, 2),
                 QString::fromUtf8("Синхронизация проектов: подключено 2 компьютера"));
        QCOMPARE(QCoreApplication::translate("SwarmController", text, nullptr, 5),
                 QString::fromUtf8("Синхронизация проектов: подключено 5 компьютеров"));
    }
    void optionalSyntheticPreview() {
        const auto destination = qEnvironmentVariable("ZERUS_LANGUAGE_PREVIEW");
        if (destination.isEmpty()) return;
        QSettings().setValue("workspace/language", "ru");
        QVERIFY(installZerusTranslation(*qApp, translator, qtTranslator));
        QWidget pane;
        auto *layout = new QVBoxLayout(&pane);
        for (const auto *text : {"Settings", "Appearance", "Interface language"})
            layout->addWidget(new QLabel(QCoreApplication::translate("SettingsPage", text)));
        layout->addWidget(zerusLanguageChoice());
        auto *hint = new QLabel(QCoreApplication::translate("SettingsPage",
            "Restart Zerus to apply the interface language. Agent sessions keep running."));
        hint->setWordWrap(true);
        layout->addWidget(hint);
        auto *preview = new QLabel(QCoreApplication::translate("SettingsPage",
            "Russian is a preview. Untranslated text stays in English."));
        preview->setWordWrap(true);
        layout->addWidget(preview);
        pane.resize(520, 280);
        pane.show();
        QTest::qWait(20);
        QVERIFY(pane.grab().save(destination));
    }
};

QTEST_MAIN(UiLanguageTest)
#include "test_ui_language.moc"

#include "Settings.h"
#include "SpeechComposer.h"
#include "SpeechClient.h"
#include "SpeechRecorder.h"
#include "RoundVideo.h"
#include <QPlainTextEdit>
#include <QThreadPool>
#include <QRunnable>
#include <QPointer>
#include "ComposerToolbar.h"
#include "AttachmentViewer.h"
#include <QApplication>
#include <QAudioOutput>
#include <QComboBox>
#include <QCryptographicHash>
#include <QDesktopServices>
#include <QDialog>
#include <QEvent>
#include <QHBoxLayout>
#include <QJsonArray>
#include <QKeyEvent>
#include <QLabel>
#include <QMediaPlayer>
#include <QMessageBox>
#include <QPushButton>
#include <QRegularExpression>
#include <QSettings>
#include <QTimer>
#include <QUuid>
#include <QVBoxLayout>
#include <QtEndian>
#include <cmath>
#include <algorithm>
#include <QJsonDocument>

namespace {
QByteArray wav(const QByteArray &pcm) {
    QByteArray result("RIFF",4);
    const auto u32=[&](quint32 value){char bytes[4];qToLittleEndian(value,bytes);result.append(bytes,4);};
    const auto u16=[&](quint16 value){char bytes[2];qToLittleEndian(value,bytes);result.append(bytes,2);};
    u32(pcm.size()+36);result.append("WAVEfmt ",8);u32(16);u16(1);u16(1);u32(16000);u32(32000);u16(2);u16(16);
    result.append("data",4);u32(pcm.size());result.append(pcm);return result;
}
QString duration(int seconds) { return QString("%1:%2").arg(seconds/60).arg(seconds%60,2,10,QLatin1Char('0')); }
QJsonObject preferredProfile() {
    const QSettings settings;const auto machine=settings.value("speech/machine_id").toString();const auto profile=settings.value("speech/profile").toString();
    return machine.isEmpty() && profile.isEmpty()?QJsonObject{}:QJsonObject{{"machine_id",machine},{"profile_id",profile}};
}
}

SpeechComposer::SpeechComposer(const QString &executable, MessageComposer *composer, std::function<Target()> target,
                               SpeechRecorder *recorder, std::function<bool(const QUrl &)> openSettings, RoundVideoRecorder *videoRecorder)
    : QObject(composer), m_composer(composer), m_client(new SpeechClient(executable,this)),
      m_recorder(recorder ? recorder : new SpeechRecorder(this)), m_currentTarget(std::move(target)),
      m_openSettings(openSettings ? std::move(openSettings) : QDesktopServices::openUrl)
{
    m_recorder->setParent(this);
    m_voice=new ToolbarChip;m_voice->setObjectName("voiceRecord");m_voice->setIconName("microphone");
    // Keep the switch icon-only at every toolbar width; update() supplies its accessible name.
    m_mode=new ToolbarChip;m_mode->setObjectName("voiceMode");m_mode->setTone(ChipTone::Quiet);
    m_cancel=new ToolbarChip;m_cancel->setObjectName("voiceCancel");m_cancel->setIconName("close");m_cancel->setLabels(tr("Cancel recording"),tr("Cancel"));
    m_result=new ToolbarChip;m_result->setObjectName("voiceResult");m_result->setIconName("microphone");
    composer->toolbar()->add(ComposerToolbar::Slot::Voice,m_voice);
    composer->toolbar()->add(ComposerToolbar::Slot::VoiceMode,m_mode);
    composer->toolbar()->add(ComposerToolbar::Slot::VoiceCancel,m_cancel);
    composer->toolbar()->add(ComposerToolbar::Slot::VoiceResult,m_result);
    m_preview=new QWidget(composer);m_preview->setObjectName("voicePreview");
    auto *row=new QHBoxLayout(m_preview);row->setContentsMargins(0,4,0,4);
    m_previewLabel=new QLabel;m_previewLabel->setTextFormat(Qt::PlainText);row->addWidget(m_previewLabel);
    m_play=new QPushButton(tr("Play","media playback"));m_play->setObjectName("voicePreviewPlay");row->addWidget(m_play);
    m_provider=new QComboBox;m_provider->setObjectName("voiceProvider");m_provider->setSizeAdjustPolicy(QComboBox::AdjustToMinimumContentsLengthWithIcon);m_provider->setMinimumContentsLength(12);row->addWidget(m_provider,1);
    m_refresh=new QPushButton(tr("Refresh"));m_refresh->setObjectName("voiceRefreshProviders");m_refresh->setToolTip(tr("Refresh available voice providers without recording again"));row->addWidget(m_refresh);
    m_discard=new QPushButton(tr("Discard"));m_discard->setObjectName("voiceDiscard");row->addWidget(m_discard);
    m_send=new QPushButton(tr("Send voice"));m_send->setObjectName("voiceSend");row->addWidget(m_send);
    qobject_cast<QVBoxLayout *>(composer->layout())->insertWidget(0,m_preview);
    m_recordingHint=new QLabel(tr("Space / Esc to stop"),composer);m_recordingHint->setObjectName("voiceRecordingHint");
    m_recordingHint->setContentsMargins(0,4,0,4);m_recordingHint->setTextFormat(Qt::PlainText);
    qobject_cast<QVBoxLayout *>(composer->layout())->insertWidget(0,m_recordingHint);
    for(auto *button:{m_play,m_discard,m_send,m_refresh})button->setAutoDefault(false);
    m_videoMode=QSettings().value("speech/video_mode",false).toBool();
    m_videoView=new RoundVideoView(composer);m_videoView->setObjectName("voiceVideoPreview");
    qobject_cast<QVBoxLayout *>(composer->layout())->insertWidget(0,m_videoView,0,Qt::AlignHCenter);
    m_videoRecorder=videoRecorder?videoRecorder:new RoundVideoRecorder(m_videoView,this);m_videoRecorder->setParent(this);
    connect(m_videoRecorder,&RoundVideoRecorder::started,this,[this]{if(m_state==State::Starting){m_state=State::Recording;update();}});
    connect(m_videoRecorder,&RoundVideoRecorder::preparing,this,[this]{if(m_captureVideo && m_state==State::Recording){m_state=State::Saving;update();}});
    connect(m_videoRecorder,&RoundVideoRecorder::elapsed,this,[this](int seconds){m_seconds=seconds;update();});
    connect(m_videoRecorder,&RoundVideoRecorder::completed,this,[this](const QByteArray &pcm,const QByteArray &mp4,const QJsonObject &meta){
        if(m_state!=State::Saving && m_state!=State::Recording)return;m_video=mp4;m_videoMeta=meta;recorded(pcm.toBase64());
    });
    connect(m_videoRecorder,&RoundVideoRecorder::failed,this,[this](const QString &error){if(m_captureVideo)fail(error);});
    m_stopPreview=new QPushButton(tr("Stop and preview"),composer);m_stopPreview->setObjectName("voiceStopPreview");
    qobject_cast<QVBoxLayout *>(composer->layout())->insertWidget(0,m_stopPreview,0,Qt::AlignLeft);
    connect(m_stopPreview,&QPushButton::clicked,this,&SpeechComposer::stopRecording);
    connect(m_voice,&QPushButton::clicked,this,[this]{if(m_state==State::Recording)requestSendRecording();else start();});
    connect(m_mode,&QPushButton::clicked,this,[this]{
        if(m_loading || m_state!=State::Idle)return;
        m_videoMode=!m_videoMode;Settings::set("speech/video_mode",m_videoMode);update();
    });
    connect(m_cancel,&QPushButton::clicked,this,&SpeechComposer::discard);
    connect(m_result,&QPushButton::clicked,this,&SpeechComposer::showResult);
    connect(m_send,&QPushButton::clicked,this,[this]{if(m_state==State::Uncertain)check();else send();});
    connect(m_discard,&QPushButton::clicked,this,&SpeechComposer::discard);
    connect(m_play,&QPushButton::clicked,this,&SpeechComposer::play);
    connect(m_refresh,&QPushButton::clicked,this,&SpeechComposer::loadProfiles);
    connect(m_provider,&QComboBox::currentIndexChanged,this,[this]{
        if(m_syncing || !m_rejection.isEmpty())return;
        const auto profile=m_provider->currentData().toJsonObject();
        if(!profile.isEmpty()){m_selectedProfile=profile;m_profileBound=true;m_profileError.clear();Settings::set("speech/machine_id",profile.value("machine_id").toString());Settings::set("speech/profile",profile.value("profile_id").toString());Settings::set("speech/profile_revision",profile.value("profile_revision").toVariant());saveProfileSelection();}
        update();
    });
    connect(composer,&MessageComposer::controlsChanged,this,[this]{syncTarget();update();});
    connect(m_recorder,&SpeechRecorder::started,this,[this]{if(m_state!=State::Starting)return;m_state=State::Recording;m_seconds=0;update();});
    connect(m_recorder,&SpeechRecorder::elapsed,this,[this](int seconds){m_seconds=seconds;update();});
    connect(m_recorder,&SpeechRecorder::recorded,this,[this](const QByteArray &audio){if(m_state==State::Saving || m_state==State::Recording)recorded(audio);});
    connect(m_recorder,&SpeechRecorder::failed,this,[this](SpeechRecorder::Failure reason,const QString &error){
        if(m_state!=State::Starting && m_state!=State::Recording && m_state!=State::Saving)return;
        const bool denied=reason==SpeechRecorder::Failure::PermissionDenied;fail(error,denied?Notice::MicrophonePermission:Notice::General);
        if(denied){const auto generation=m_generation;QTimer::singleShot(0,this,[this,generation]{if(generation==m_generation && m_noticeKind==Notice::MicrophonePermission && m_composer->isVisible())showPermissionRecovery();});}
    });
    qApp->installEventFilter(this);syncTarget();update();
}
QString SpeechComposer::storageKey() const {return "voice-message-draft\n"+m_loadedKey;}
QJsonObject SpeechComposer::identityPayload(const QString &action) const {
    return {{"action",action},{"request_id",QUuid::createUuid().toString(QUuid::WithoutBraces)},
        {"expected_run_id",m_target.run},{"expected_conversation_id",m_target.conversation}};
}
bool SpeechComposer::eventFilter(QObject *watched,QEvent *event) {
    if((watched==m_composer && event->type()==QEvent::Hide) || event->type()==QEvent::ApplicationDeactivate){
        m_sendAfterSaved=false;stopPlayback();
        if(m_state==State::Starting || m_state==State::Recording)stopRecording();
    }
    if(event->type()==QEvent::ShortcutOverride || event->type()==QEvent::KeyPress || event->type()==QEvent::KeyRelease){
        auto *widget=qobject_cast<QWidget *>(watched);auto *key=static_cast<QKeyEvent *>(event);
        // Window-scoped, before Qt chooses the agent's Escape shortcut. Other
        // windows and modal dialogs retain their own keyboard handling.
        if(widget && widget->window()==m_composer->window()){
            // A fresh physical press also recovers a release lost while the
            // application was inactive. Only auto-repeat belongs to the old press.
            if(event->type()!=QEvent::KeyRelease && !key->isAutoRepeat())m_stopKeys.remove(key->key());
            const bool stoppingKey=key->key()==Qt::Key_Space || key->key()==Qt::Key_Escape;
            const bool capture=m_state==State::Starting || m_state==State::Recording;
            if(m_stopKeys.contains(key->key()) || (capture && stoppingKey && key->modifiers()==Qt::NoModifier)){
                event->accept();
                if(event->type()==QEvent::KeyPress && !key->isAutoRepeat() && !m_stopKeys.contains(key->key())){
                    m_stopKeys.insert(key->key());stopRecording();
                }else if(event->type()==QEvent::KeyRelease && !key->isAutoRepeat())m_stopKeys.remove(key->key());
                return true;
            }
        }else if(event->type()==QEvent::KeyRelease && !key->isAutoRepeat())m_stopKeys.remove(key->key());
    }
    return QObject::eventFilter(watched,event);
}
void SpeechComposer::stopRecording() {
    if(m_state==State::Starting){++m_generation;m_recorder->cancel();m_videoRecorder->cancel();m_sendAfterSaved=false;m_state=State::Idle;update();return;}
    if(m_state!=State::Recording)return;
    m_state=State::Saving;update();if(m_captureVideo)m_videoRecorder->stop();else m_recorder->stop();
}
void SpeechComposer::syncTarget() {
    if(m_syncing)return;
    const auto key=m_composer->sessionKey();if(key==m_loadedKey)return;
    m_sendAfterSaved=false;
    ++m_generation;m_recorder->cancel();m_videoRecorder->cancel();stopPlayback();m_loadedKey=key;m_target=m_currentTarget();
    m_rejection={};m_attemptedRejectionGeneration=0;m_operationId.clear();
    m_pcm.clear();m_transcript.clear();m_video.clear();m_videoMeta={};m_captureVideo=false;m_id.clear();m_notice.clear();m_noticeKind=Notice::General;m_state=State::Idle;m_loading=!key.isEmpty();
    m_profilesLoading=false;m_profilesValidated=false;m_profileError.clear();m_selectedProfile=preferredProfile();m_profileBound=false;
    m_syncing=true;m_provider->clear();m_syncing=false;
    if(key.isEmpty())return;
    const auto generation=m_generation;
    m_store.loadAsync(storageKey(),this,[this,generation](const ComposerDraft &draft){
        if(generation!=m_generation)return;m_loading=false;
        if(!draft.attachments.isEmpty()){
            const auto meta=draft.formState;
            if(!meta.value("speech").toObject().isEmpty()){m_selectedProfile=meta.value("speech").toObject();m_profileBound=true;}
            m_target={m_loadedKey,meta.value("host").toString(),meta.value("session").toString(),meta.value("run").toString(),meta.value("conversation").toString(),{},false};
            m_target.draftOnly=meta.value("draft_only").toBool(); m_transcript=meta.value("transcript").toString();
            const auto audio=draft.attachments.first().data;
            if(!audio.startsWith("RIFF") || audio.mid(8,8)!="WAVEfmt " || audio.mid(36,4)!="data" || audio.size()<3244){m_notice=tr("The saved voice recording is invalid.");update();return;}
            m_videoMeta=meta.value("video").toObject();
            if(!m_videoMeta.isEmpty() && draft.attachments.size()==2){m_video=draft.attachments[1].data;m_captureVideo=true;}
            m_pcm=audio.mid(44);m_id=meta.value("id").toString();m_seconds=m_pcm.size()/32000;
            m_operationId=meta.value("operation_id").toString(m_id);
            m_rejection=meta.value("rejection").toObject();m_attemptedRejectionGeneration=meta.value("attempted_rejection_generation").toInteger();
            m_state=meta.value("submitted").toBool()?State::Uncertain:State::Preview;
            if(m_state==State::Uncertain)m_notice=tr("Submission was interrupted. Check this message; audio is never sent twice automatically.");
            loadProfiles();
        }
        update();
    });
}
void SpeechComposer::loadProfiles() {
    if(m_profilesLoading)return;
    const auto generation=m_generation;const auto request=++m_profileGeneration;const auto target=m_target;
    m_profilesLoading=true;m_profilesValidated=false;m_profileError.clear();update();
    const auto receive=[this,generation,request,target](const QJsonObject &reply,const QString &error){
        if(generation!=m_generation || request!=m_profileGeneration)return;
        m_profilesLoading=false;
        if(!m_currentTarget().sameConversation(target) || !m_target.sameConversation(target)){
            m_profileError=tr("The conversation changed. The recording is kept without sending.");update();return;
        }
        if(!error.isEmpty()){m_profileError=error;update();return;}
        const auto preferredMachine=m_selectedProfile.value("machine_id").toString();const auto preferredId=m_selectedProfile.value("profile_id").toString();
        int selected=-1,profileMatch=-1,profileMatches=0;
        m_syncing=true;m_provider->clear();
        for(const auto &computerValue:reply.value("computers").toArray()){
            const auto computer=computerValue.toObject();const auto machine=computer.value("machine_id").toString();
            for(const auto &value:computer.value("profiles").toArray()){
                const auto profile=value.toObject();if(!profile.value("available").toBool() || QUuid(machine).isNull() || QUuid(machine).toString(QUuid::WithoutBraces)!=machine)continue;
                const auto id=profile.value("id").toString();if(!QRegularExpression("^[A-Za-z0-9_-]{1,64}$").match(id).hasMatch())continue;
                QJsonObject data{{"machine_id",machine},{"profile_id",id}};
                if(profile.contains("profile_revision")){
                    const auto revision=profile.value("profile_revision").toDouble(-1);
                    if(!profile.value("profile_revision").isDouble() || revision<1 || revision>9007199254740991. || std::floor(revision)!=revision)continue;
                    data["profile_revision"]=profile.value("profile_revision");
                }
                m_provider->addItem(computer.value("name").toString()+" / "+profile.value("name").toString(id),QVariant::fromValue(data));
                const auto index=m_provider->count()-1;
                if(machine==preferredMachine && id==preferredId && (!m_profileBound || data.value("profile_revision")==m_selectedProfile.value("profile_revision")))selected=index;
                if(preferredMachine.isEmpty() && (!m_profileBound || data.value("profile_revision")==m_selectedProfile.value("profile_revision")) && id==preferredId){profileMatch=index;++profileMatches;}
            }
        }
        if(selected<0 && profileMatches==1)selected=profileMatch;
        if(selected<0 && m_selectedProfile.isEmpty() && m_provider->count())selected=0;
        if(selected>=0){m_provider->setCurrentIndex(selected);m_selectedProfile=m_provider->currentData().toJsonObject();m_profileBound=true;}
        else if(!m_selectedProfile.isEmpty()){
            m_provider->addItem(tr("Selected provider (unavailable)"),QVariant::fromValue(QJsonObject{}));
            m_provider->setCurrentIndex(m_provider->count()-1);
        }
        m_syncing=false;
        m_videoSupported=reply.value("features").toArray().contains("voice_video");
        m_profilesValidated=true;
        if(selected<0)m_profileError=m_selectedProfile.isEmpty()?tr("No speech provider is available. Refresh to try again; your recording is kept.")
            :tr("The selected voice provider is unavailable or its configuration changed. Refresh or explicitly choose a provider; your recording is kept.");
        if(m_captureVideo && !m_videoSupported)m_profileError=tr("Update hgs on the conversation computer to send video messages.");
        saveProfileSelection();update();
    };
    if (target.draftOnly) m_client->request({}, {{"action", "draft_profiles"}}, receive);
    else m_client->requestVoice(target.host,target.session,identityPayload("profiles"),receive);
}
QJsonObject SpeechComposer::selectedProfile() const {
    const auto selected=m_provider->currentData().toJsonObject();
    return m_profilesValidated && !m_profilesLoading && selected==m_selectedProfile && !selected.value("machine_id").toString().isEmpty()
        && !selected.value("profile_id").toString().isEmpty()?selected:QJsonObject{};
}
void SpeechComposer::saveProfileSelection() {
    if(m_pcm.isEmpty() || (m_state!=State::Preview && m_state!=State::Saving))return;
    saveVoice([this](bool okay){if(!okay){m_notice=tr("Could not save the voice provider selection. Keep Zerus open and try again.");update();}});
}
void SpeechComposer::start() {
    syncTarget();const auto target=m_currentTarget();
    if(m_loading || m_state!=State::Idle || !target.available || (!target.draftOnly && (target.run.isEmpty() || target.conversation.isEmpty())) || !m_composer->isInputVisible())return;
    pauseMessagePlayback();
    m_video.clear();m_videoMeta={};m_captureVideo=!target.draftOnly && m_videoMode;m_sendAfterSaved=false;
    m_previewBeforeSend=target.draftOnly || QSettings().value("speech/preview_before_send",false).toBool();
    m_target=target;m_transcript.clear();m_notice.clear();m_noticeKind=Notice::General;m_state=State::Starting;++m_generation;
    m_selectedProfile=preferredProfile();m_profileBound=false;m_profilesLoading=false;m_profilesValidated=false;m_profileError.clear();
    m_syncing=true;m_provider->clear();m_syncing=false;update();
    if(m_captureVideo)m_videoRecorder->start(QSettings().value("speech/microphone").toByteArray());
    else m_recorder->start(QSettings().value("speech/microphone").toByteArray());
    loadProfiles();
}
void SpeechComposer::recorded(const QByteArray &base64) {
    const auto decoded=QByteArray::fromBase64Encoding(base64,QByteArray::AbortOnBase64DecodingErrors);
    if(!decoded || decoded.decoded.size()<3200 || decoded.decoded.size()>SpeechPcmConverter::MaximumBytes){fail(tr("The voice recording is invalid."));return;}
    m_pcm=decoded.decoded;m_seconds=m_pcm.size()/32000;m_id=QUuid::createUuid().toString(QUuid::WithoutBraces);m_state=State::Saving;
    update();
    const bool sendAfter=m_sendAfterSaved;m_sendAfterSaved=false;
    saveVoice([this,sendAfter](bool saved){m_state=State::Preview;if(!saved)m_notice=tr("Could not save this recording. Keep Zerus open and try Send again.");update();if(saved && sendAfter)send();});
}
void SpeechComposer::requestSendRecording(){
    if(m_state!=State::Recording)return;
    if(m_previewBeforeSend){m_sendAfterSaved=false;stopRecording();return;}
    // A Send click never waits for late provider discovery to become a future Send.
    m_sendAfterSaved=!selectedProfile().isEmpty() && (!m_captureVideo || m_videoSupported);
    if(!m_sendAfterSaved)m_notice=m_captureVideo && !m_videoSupported?tr("Update hgs on the conversation computer to send video messages."):tr("Review the transcription provider before sending. The recording is kept.");
    stopRecording();
}
void SpeechComposer::saveVoice(std::function<void(bool)> done,bool requireGeneration) {
    ComposerDraft draft;draft.label=tr("Voice message (%1)").arg(m_target.session);
    draft.notice=tr("Open the original conversation to review or check this voice message.");
    draft.attachments.append({"voice.wav","audio/wav",wav(m_pcm),{}});
    draft.formState={{"kind","voice_message"},{"id",m_id},{"host",m_target.host},{"session",m_target.session},{"run",m_target.run},{"conversation",m_target.conversation},
        {"speech",m_selectedProfile},{"draft_only",m_target.draftOnly},{"transcript",m_transcript},{"submitted",m_state==State::Submitting || m_state==State::Uncertain},
        {"rejection",m_rejection},{"attempted_rejection_generation",m_attemptedRejectionGeneration},{"operation_id",m_operationId}};
    if(!m_video.isEmpty()){draft.attachments.append({"video.mp4","video/mp4",m_video,{}});draft.formState["video"]=m_videoMeta;}
    const auto generation=m_generation;
    m_store.saveDurableAsync(storageKey(),draft,this,[this,generation,done,requireGeneration](bool okay,const ComposerDraft &){if(!requireGeneration || generation==m_generation)done(okay);});
}
void SpeechComposer::send() {
    if (m_target.draftOnly) {
        if (m_state != State::Preview) return;
        if (!m_transcript.isEmpty()) { m_draftTarget = m_composer->voiceTarget(); applyTranscript(); return; }
        if (selectedProfile().isEmpty() || !m_currentTarget().sameConversation(m_target) || !m_currentTarget().available) return;
        m_draftTarget = m_composer->voiceTarget(); m_draftPolls = 0;
        m_state = State::Submitting; m_notice.clear(); update();
        saveVoice([this](bool saved) {
            if (!saved) { m_state = State::Preview; m_notice = tr("Could not save the transcription request. No audio was sent."); update(); return; }
            draftRequest(true);
        }); return;
    }
    const auto chosen=selectedProfile();
    if(m_state!=State::Preview || m_pcm.isEmpty() || chosen.isEmpty())return;
    if(!m_video.isEmpty() && !m_videoSupported){m_notice=tr("Update hgs on the conversation computer to send video messages.");update();return;}
    const auto current=m_currentTarget();
    if(!current.available || !current.sameConversation(m_target)){m_notice=tr("The conversation changed or cannot receive input. The recording is kept without sending.");update();return;}
    if(!current.warning.isEmpty()){
        const auto originalId=m_id;const auto originalGeneration=m_generation;const auto originalTarget=m_target;
        QMessageBox confirm(QMessageBox::Warning,tr("Continue with a cold cache?"),current.warning,QMessageBox::NoButton,m_composer);
        confirm.setObjectName("voiceColdCacheConfirm");
        auto *send=confirm.addButton(tr("Send with full context"),QMessageBox::AcceptRole);send->setObjectName("confirmVoiceSend");
        auto *cancel=confirm.addButton(QMessageBox::Cancel);confirm.setDefaultButton(cancel);confirm.setEscapeButton(cancel);confirm.exec();
        const auto after=m_currentTarget();
        if(confirm.clickedButton()!=send || m_generation!=originalGeneration || m_id!=originalId || m_state!=State::Preview
            || !m_target.sameConversation(originalTarget) || !after.available || !after.sameConversation(originalTarget) || selectedProfile()!=chosen)return;
    }
    stopPlayback();m_notice.clear();m_state=State::Submitting;update();
    const auto generation=m_generation;auto payload=identityPayload("submit");payload["request_id"]=m_id;payload["speech"]=chosen;
    const auto previousAttempt=m_attemptedRejectionGeneration;
    if(!m_rejection.isEmpty()){
        payload["request_id"]=QUuid::createUuid().toString(QUuid::WithoutBraces);payload["message_id"]=m_id;payload["rejection"]=m_rejection;
        m_attemptedRejectionGeneration=m_rejection.value("generation").toInteger();
    }
    m_operationId=payload.value("request_id").toString();
    Settings::set("speech/machine_id",chosen.value("machine_id").toString());Settings::set("speech/profile",chosen.value("profile_id").toString());Settings::set("speech/profile_revision",chosen.value("profile_revision").toVariant());
    payload["audio"]=QJsonObject{{"format","pcm_s16le"},{"sample_rate",16000},{"channels",1},{"data_base64",QString::fromLatin1(m_pcm.toBase64())}};
    const auto target=m_target;const auto key=storageKey();
    const auto video=m_video;const auto videoMeta=m_videoMeta;
    saveVoice([this,generation,payload,target,key,video,videoMeta,previousAttempt](bool saved) mutable {
        if(!saved){if(generation==m_generation){m_attemptedRejectionGeneration=previousAttempt;m_state=State::Preview;m_notice=tr("Could not save the voice request. No audio was sent.");update();}return;}
        // Explicit Send owns this immutable target even if navigation changes
        // while the local checkpoint is being written.
        if(!video.isEmpty()){
            const QPointer<SpeechComposer> guard(this);
            QThreadPool::globalInstance()->start(QRunnable::create([guard,generation,payload,target,key,video,videoMeta]() mutable {
                auto media=videoMeta;media["data_base64"]=QString::fromLatin1(video.toBase64());payload["video"]=media;
                if(guard)QMetaObject::invokeMethod(guard,[guard,generation,payload,target,key]{if(!guard)return;
                    guard->m_client->requestVoice(target.host,target.session,payload,[guard,generation,target,key,payload](const QJsonObject &reply,const QString &error){
                        if(!guard)return;
                        if(generation==guard->m_generation){guard->result(reply,error,payload);return;}
                        const auto message=reply.value("message").toObject();
                        if(error.isEmpty() && message.value("id")==payload.value(payload.contains("message_id")?"message_id":"request_id") && message.value("run_id").toString()==target.run && message.value("conversation_id").toString()==target.conversation){
                            emit guard->messageChanged(target.key,message);guard->m_store.saveDurableAsync(key,{},guard,[](bool,const ComposerDraft&){});
                        }
                    });
                },Qt::QueuedConnection);
            }));return;
        }
        m_client->requestVoice(target.host,target.session,payload,[this,generation,target,key,payload](const QJsonObject &reply,const QString &error){
            if(generation==m_generation){result(reply,error,payload);return;}
            const auto message=reply.value("message").toObject();
            if(error.isEmpty() && message.value("id")==payload.value(payload.contains("message_id")?"message_id":"request_id") && message.value("run_id")==target.run && message.value("conversation_id")==target.conversation){
                emit messageChanged(target.key,message);m_store.saveDurableAsync(key,{},this,[](bool,const ComposerDraft&){});
            }
        });
    },false);
}
void SpeechComposer::check() {
    if (m_target.draftOnly) { if (m_state == State::Uncertain) { m_draftTarget = m_composer->voiceTarget(); m_draftPolls = 0; m_state = State::Submitting; update(); draftRequest(false); } return; }
    if(m_id.isEmpty() || m_state!=State::Uncertain)return;
    m_state=State::Submitting;update();auto payload=identityPayload("status");payload["message_id"]=m_id;const auto generation=m_generation;
    m_client->requestVoice(m_target.host,m_target.session,payload,[this,generation,payload](const QJsonObject &reply,const QString &error){if(generation==m_generation)result(reply,error,payload);});
}
void SpeechComposer::result(const QJsonObject &reply,const QString &error,const QJsonObject &request) {
    if(m_id.isEmpty() || request.value(request.contains("message_id")?"message_id":"request_id").toString()!=m_id)return;
    if(request.value("action")=="submit" && request.value("request_id").toString()!=m_operationId)return;
    const auto message=reply.value("message").toObject();
    if(error.isEmpty() && message.isEmpty() && reply.contains("rejection")){
        const auto proof=reply.value("rejection").toObject();
        const QSet<QString> expected={"schema","state","request_id","message_id","run_id","conversation_id","intent","payload_sha256","generation","retry_token","code","detail"};
        const auto keys=proof.keys();const auto generation=proof.value("generation").toInteger();
        const auto digest=[](const QJsonValue &value){return QRegularExpression("^[0-9a-f]{64}$").match(value.toString()).hasMatch();};
        const auto uuid=[](const QJsonValue &value){const auto text=value.toString();const QUuid id(text);return !id.isNull() && id.toString(QUuid::WithoutBraces)==text;};
        const auto detail=proof.value("detail").toString();
        auto intent=QJsonObject{{"speech",m_selectedProfile},{"audio",QJsonObject{{"format","pcm_s16le"},{"sample_rate",16000},{"channels",1},
            {"bytes",m_pcm.size()+44},{"sha256",QString::fromLatin1(QCryptographicHash::hash(wav(m_pcm),QCryptographicHash::Sha256).toHex())}}}};
        if(!m_video.isEmpty()){
            auto media=m_videoMeta;media.remove("poster_base64");media["bytes"]=m_video.size();
            media["sha256"]=QString::fromLatin1(QCryptographicHash::hash(m_video,QCryptographicHash::Sha256).toHex());
            if(m_videoMeta.contains("poster_base64"))media["poster_sha256"]=QString::fromLatin1(QCryptographicHash::hash(QByteArray::fromBase64(m_videoMeta.value("poster_base64").toString().toLatin1()),QCryptographicHash::Sha256).toHex());
            intent["video"]=media;
        }
        const auto oldGeneration=m_rejection.value("generation").toInteger();
        const bool valid=reply.value("schema")==1 && QSet<QString>(keys.begin(),keys.end())==expected && QJsonDocument(proof).toJson(QJsonDocument::Compact).size()<=4096
            && proof.value("schema")==1 && proof.value("state")=="rejected_before_acceptance"
            && uuid(proof.value("request_id")) && uuid(proof.value("retry_token")) && proof.value("request_id")==request.value("request_id")
            && proof.value("message_id")==m_id && proof.value("run_id")==m_target.run && proof.value("conversation_id")==m_target.conversation
            && proof.value("intent").toObject()==intent && digest(proof.value("payload_sha256"))
            && generation>m_attemptedRejectionGeneration && generation>=oldGeneration
            && (m_rejection.isEmpty() || (proof.value("payload_sha256")==m_rejection.value("payload_sha256")
                && (generation>oldGeneration || proof.value("retry_token")==m_rejection.value("retry_token"))))
            && QStringList{"route_unavailable","storage_full","queue_full"}.contains(proof.value("code").toString())
            && !detail.isEmpty() && detail.toUtf8().size()<=2000 && std::none_of(detail.begin(),detail.end(),[](QChar c){return c.category()==QChar::Other_Control;});
        if(valid){
            m_rejection=proof;m_state=State::Saving;m_notice=tr("The computer did not accept this recording. Send voice retries the same recording and provider.");update();
            saveVoice([this](bool saved){m_state=saved?State::Preview:State::Uncertain;if(!saved)m_notice=tr("Could not save the refusal. Check this message before trying again.");update();});return;
        }
    }
    if(!error.isEmpty() || message.value("id").toString()!=m_id || message.value("run_id").toString()!=m_target.run || message.value("conversation_id").toString()!=m_target.conversation){
        m_state=State::Uncertain;m_notice=error.isEmpty()?tr("Voice submission is not confirmed. Check this message before trying another recording."):error;update();return;
    }
    if(!m_video.isEmpty()){
        const auto media=message.value("video").toObject();
        if(media.value("request_id").toString()!=m_id || media.value("index").toInt(-1)!=1 || media.value("bytes").toInteger()!=m_video.size()
            || media.value("mime")!="video/mp4" || media.value("duration_ms")!=m_videoMeta.value("duration_ms")
            || media.value("width")!=m_videoMeta.value("width") || media.value("height")!=m_videoMeta.value("height")){
            m_state=State::Uncertain;m_notice=tr("Video acceptance is not confirmed. Check this message before trying again.");update();return;
        }
    }
    emit messageChanged(m_target.key,message);
    const auto generation=m_generation;
    m_store.saveDurableAsync(storageKey(),{},this,[this,generation](bool saved,const ComposerDraft &){
        if(generation!=m_generation)return;
        if(!saved){m_state=State::Uncertain;m_notice=tr("The message was accepted, but its local receipt could not be saved. Check it again.");update();return;}
        m_pcm.clear();m_transcript.clear();m_video.clear();m_videoMeta={};m_id.clear();m_rejection={};m_attemptedRejectionGeneration=0;m_operationId.clear();m_state=State::Idle;m_notice.clear();update();
    });
}
void SpeechComposer::discard() {
    if(m_state==State::Submitting || m_state==State::Saving)return;
    if(m_state==State::Uncertain && !m_target.draftOnly){
        auto payload=identityPayload("cancel");payload["message_id"]=m_id;
        const auto generation=m_generation;m_state=State::Submitting;update();
        m_client->requestVoice(m_target.host,m_target.session,payload,[this,generation,payload](const QJsonObject &reply,const QString &error){if(generation==m_generation)result(reply,error,payload);});
        return;
    }
    if(!m_pcm.isEmpty()){
        const auto originalId=m_id;const auto originalTarget=m_target;const auto originalGeneration=m_generation;
        QMessageBox dialog(QMessageBox::Warning,tr("Discard voice recording?"),
            tr("Delete the saved, unsent voice recording for %1?").arg(m_target.session),QMessageBox::NoButton,m_composer);
        dialog.setObjectName("voiceDiscardConfirm");dialog.setTextFormat(Qt::PlainText);
        auto *confirm=dialog.addButton(tr("Discard recording"),QMessageBox::DestructiveRole);confirm->setObjectName("confirmVoiceDiscard");
        auto *cancel=dialog.addButton(QMessageBox::Cancel);dialog.setDefaultButton(cancel);dialog.setEscapeButton(cancel);dialog.exec();
        if(dialog.clickedButton()!=confirm || m_generation!=originalGeneration || m_id!=originalId
            || !m_target.sameConversation(originalTarget) || (m_state!=State::Preview && !(m_target.draftOnly && m_state==State::Uncertain)))return;
        if (m_target.draftOnly) {
            ++m_generation;
            if (m_state == State::Uncertain) m_client->request({}, {{"action", "draft_cancel"}, {"machine_id", m_selectedProfile.value("machine_id")}, {"job_id", m_id}}, [](const QJsonObject &, const QString &) {});
        }
        stopPlayback();++m_profileGeneration;m_profilesLoading=false;m_profilesValidated=false;const auto generation=m_generation;m_state=State::Saving;update();
        m_store.saveDurableAsync(storageKey(),{},this,[this,generation](bool saved,const ComposerDraft&){
            if(generation!=m_generation)return;
            if(!saved){m_state=State::Preview;m_notice=tr("Could not discard the saved recording. Try again.");update();return;}
            m_pcm.clear();m_transcript.clear();m_video.clear();m_videoMeta={};m_id.clear();m_rejection={};m_attemptedRejectionGeneration=0;m_operationId.clear();m_state=State::Idle;m_notice.clear();update();
        });return;
    }
    ++m_generation;m_recorder->cancel();m_videoRecorder->cancel();m_sendAfterSaved=false;stopPlayback();m_pcm.clear();m_transcript.clear();m_video.clear();m_videoMeta={};m_id.clear();m_rejection={};m_attemptedRejectionGeneration=0;m_operationId.clear();m_state=State::Idle;m_notice.clear();m_noticeKind=Notice::General;
    if(!m_loadedKey.isEmpty())m_store.saveDurableAsync(storageKey(),{},this,[](bool,const ComposerDraft&){});update();
}
void SpeechComposer::stopPlayback(){if(m_player)m_player->stop();}
void SpeechComposer::play() {
    if(m_pcm.isEmpty())return;
    m_videoView->setMirrored(false);
    if(m_player && m_player->playbackState()==QMediaPlayer::PlayingState){m_player->pause();return;}
    if(!m_player){m_player=new QMediaPlayer(this);registerMessagePlayer(m_player);m_output=new QAudioOutput(this);m_player->setAudioOutput(m_output);m_player->setVideoSink(m_videoView->sink());
        connect(m_player,&QMediaPlayer::playbackStateChanged,this,[this]{update();});
        connect(m_player,&QMediaPlayer::errorOccurred,this,[this](QMediaPlayer::Error,const QString &error){m_notice=tr("Could not play recording: %1").arg(error);update();});}
    const auto generation=m_generation;
    AttachmentFiles::prepareAsync({m_video.isEmpty()?MessageAttachment{"voice.wav","audio/wav",wav(m_pcm),{}}:MessageAttachment{"video.mp4","video/mp4",m_video,{}}},this,[this,generation](const QJsonArray &files,const QString &error){
        if(generation!=m_generation || m_pcm.isEmpty())return;
        if(!error.isEmpty() || files.isEmpty()){m_notice=error;update();return;}
        const auto url=QUrl::fromLocalFile(files.first().toObject().value("local_path").toString());if(m_player->source()!=url)m_player->setSource(url);m_player->play();
    });
}
void SpeechComposer::fail(const QString &error,Notice notice){m_sendAfterSaved=false;m_state=State::Idle;m_notice=error;m_noticeKind=notice;update();}
void SpeechComposer::update() {
    const auto current=m_currentTarget();const bool recording=m_state==State::Recording;
    m_composer->setRecording(recording || m_state==State::Starting || (m_target.draftOnly && m_state==State::Saving));
    m_recordingHint->setVisible(recording);
    m_recordingHint->setText(m_previewBeforeSend?tr("Recording. Stop to preview. Space / Esc to stop")
        :tr("Recording. Send, or Stop to preview. Space / Esc to stop"));
    m_stopPreview->setVisible(recording);
    m_videoView->setVisible(m_captureVideo && (m_state==State::Starting || recording || !m_pcm.isEmpty()));
    m_voice->setActive(true);m_voice->setEnabled(!m_loading && (recording || (m_state==State::Idle && current.available && (current.draftOnly || !current.conversation.isEmpty()) && m_composer->isInputVisible())));
    m_voice->setTone(recording?ChipTone::Danger:ChipTone::Quiet);m_voice->setIconName(recording?(m_previewBeforeSend?"stop":"send"):(!current.draftOnly && m_videoMode?"video":"microphone"));
    m_voice->setLabels(recording?(m_previewBeforeSend?tr("Stop (%1)"):tr("Send (%1)")).arg(duration(m_seconds)):m_state==State::Starting?tr("Opening recorder…"):m_state==State::Saving?tr("Saving recording…"):(!current.draftOnly && m_videoMode?tr("Video"):tr("Voice")));
    m_voice->setDetail(!recording && !current.available && !current.reason.isEmpty()?current.reason
        :recording?(m_previewBeforeSend?tr("Stop recording and preview before sending."):tr("Send recording. Stop and preview or Space / Esc keeps it for review."))
        :m_videoMode?tr("Click to start a video recording."):tr("Click to start a voice recording."));
    m_mode->setActive(!current.draftOnly);m_mode->setEnabled(!m_loading && m_state==State::Idle);
    m_mode->setIconName(m_videoMode?"microphone":"video");
    const auto modeLabel=m_videoMode?tr("Switch to voice recording"):tr("Switch to video recording");
    m_mode->setAccessibleName(modeLabel);m_mode->setDetail(modeLabel);
    m_cancel->setActive(m_state==State::Starting || recording);
    const bool preview=!m_pcm.isEmpty();m_preview->setVisible(preview);m_previewLabel->setText(tr("Voice %1").arg(duration(m_seconds)));
    m_play->setText(m_player && m_player->playbackState()==QMediaPlayer::PlayingState?tr("Pause","media playback"):tr("Play","media playback"));
    m_play->setEnabled(preview && m_state!=State::Submitting);m_discard->setEnabled(m_state==State::Preview || m_state==State::Uncertain);m_provider->setEnabled(m_state==State::Preview && m_rejection.isEmpty() && m_profilesValidated && !m_profilesLoading);
    m_refresh->setEnabled(m_state==State::Preview && !m_profilesLoading);m_refresh->setText(m_profilesLoading?tr("Checking…"):tr("Refresh"));
    m_provider->setPlaceholderText(m_profilesLoading?tr("Checking providers…"):tr("Choose a voice provider"));m_provider->setToolTip(m_profileError);
    m_discard->setText(m_state==State::Uncertain && !current.draftOnly?tr("Cancel message"):tr("Discard"));
    m_send->setText(current.draftOnly ? (m_state==State::Uncertain ? tr("Check transcription") : m_state==State::Submitting ? tr("Transcribing…") : m_transcript.isEmpty() ? tr("Transcribe") : tr("Add to draft"))
        : m_state==State::Uncertain?tr("Check message"):m_state==State::Submitting?tr("Sending voice…"):tr("Send voice"));
    m_send->setEnabled(m_state==State::Uncertain || (m_state==State::Preview && current.available && current.sameConversation(m_target)
        && ((current.draftOnly && !m_transcript.isEmpty()) || !selectedProfile().isEmpty()) && (m_video.isEmpty() || m_videoSupported)));
    m_result->setActive(!m_notice.isEmpty() || !m_profileError.isEmpty());m_result->setLabels(m_noticeKind==Notice::MicrophonePermission?tr("Microphone access"):tr("Voice notice"));m_result->setDetail(m_notice.isEmpty()?m_profileError:m_notice);
}
void SpeechComposer::showResult() {
    if(m_noticeKind==Notice::MicrophonePermission){showPermissionRecovery();return;}
    QDialog dialog(m_composer);dialog.setObjectName("voiceResultDialog");dialog.setWindowTitle(tr("Voice message"));
    auto *layout=new QVBoxLayout(&dialog);layout->setSizeConstraint(QLayout::SetFixedSize);
    auto *hint=new QLabel(m_notice.isEmpty()?m_profileError:m_notice);hint->setWordWrap(true);hint->setFixedWidth(420);layout->addWidget(hint);
    auto *close=new QPushButton(tr("Close"));layout->addWidget(close,0,Qt::AlignRight);connect(close,&QPushButton::clicked,&dialog,&QDialog::accept);dialog.exec();
}
QUrl SpeechComposer::microphoneSettingsUrl()
{
#ifdef Q_OS_MACOS
    return QUrl(QStringLiteral("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"));
#elif defined(Q_OS_WIN)
    return QUrl(QStringLiteral("ms-settings:privacy-microphone"));
#else
    return {};
#endif
}
void SpeechComposer::showPermissionRecovery()
{
    QDialog dialog(m_composer); dialog.setObjectName("voicePermissionDialog"); dialog.setWindowTitle(tr("Microphone access"));
    auto *layout = new QVBoxLayout(&dialog); layout->setSpacing(16); layout->setSizeConstraint(QLayout::SetFixedSize);
#ifdef Q_OS_MACOS
    const auto instructions = tr("Allow Zerus to use the microphone in System Settings > Privacy & Security > Microphone. Then return to Zerus and press Voice again.");
#elif defined(Q_OS_WIN)
    const auto instructions = tr("Open Settings > Privacy & security > Microphone. Enable Microphone access and Let desktop apps access your microphone. Then return to Zerus and press Voice again.");
#else
    const auto instructions = tr("Open your desktop's Settings and check microphone permissions for Zerus. Then return to Zerus and press Voice again.");
#endif
    auto *hint = new QLabel(instructions); hint->setObjectName("voicePermissionHint"); hint->setWordWrap(true); hint->setFixedWidth(420); layout->addWidget(hint);
    auto *row = new QHBoxLayout; layout->addLayout(row);
    const auto settings = microphoneSettingsUrl();
    if (!settings.isEmpty()) {
        auto *open = new QPushButton(tr("Open microphone settings")); open->setObjectName("voiceOpenMicrophoneSettings"); row->addWidget(open);
        connect(open,&QPushButton::clicked,&dialog,[this,&dialog,hint,instructions,settings] {
            if (m_openSettings(settings)) dialog.accept();
            else hint->setText(tr("Could not open system settings automatically.\n\n%1").arg(instructions));
        });
    }
    row->addStretch(); auto *close = new QPushButton(tr("Close")); close->setObjectName("voicePermissionClose"); row->addWidget(close);
    connect(close,&QPushButton::clicked,&dialog,&QDialog::accept); dialog.exec();
}

void SpeechComposer::draftRequest(bool submit) {
    if (m_id.isEmpty() || !m_target.draftOnly) return;
    QJsonObject payload{{"action", submit ? "draft_submit" : "draft_status"}, {"machine_id", m_selectedProfile.value("machine_id")}};
    if (submit) {
        payload["request_id"] = m_id; payload["profile_id"] = m_selectedProfile.value("profile_id");
        if (m_selectedProfile.contains("profile_revision")) payload["profile_revision"] = m_selectedProfile.value("profile_revision");
        payload["audio"] = QJsonObject{{"format", "pcm_s16le"}, {"sample_rate", 16000}, {"channels", 1}, {"data_base64", QString::fromLatin1(m_pcm.toBase64())}};
    } else payload["job_id"] = m_id;
    const auto generation = m_generation;
    m_client->request({}, payload, [this, generation](const QJsonObject &reply, const QString &error) {
        if (generation == m_generation) draftResult(reply, error);
    });
}
void SpeechComposer::draftResult(const QJsonObject &reply, const QString &error) {
    if (!m_target.draftOnly || m_id.isEmpty()) return;
    if (!error.isEmpty() || reply.value("schema") != 1 || reply.value("job_id").toString() != m_id) {
        m_state = State::Uncertain; m_notice = error.isEmpty() ? tr("Transcription is unconfirmed. Check the existing request.") : error; update(); return;
    }
    const auto state = reply.value("state").toString();
    if (state == QLatin1String("queued") || state == QLatin1String("running")) {
        if (++m_draftPolls >= 120) { m_state = State::Uncertain; m_notice = tr("Transcription is still pending. Check it later; the recording is kept."); update(); return; }
        const auto generation = m_generation;
        QTimer::singleShot(1000, this, [this, generation] { if (generation == m_generation && m_state == State::Submitting) draftRequest(false); }); return;
    }
    if (state != QLatin1String("completed") || reply.value("text").toString().trimmed().isEmpty() || reply.value("text").toString().toUtf8().size() > 100000) {
        m_state = State::Uncertain; m_notice = tr("The recording has no confirmed transcript. The audio is kept."); update(); return;
    }
    m_transcript = reply.value("text").toString(); m_state = State::Saving; update();
    saveVoice([this](bool saved) {
        m_state = State::Preview; update();
        if (saved) applyTranscript(); else { m_notice = tr("Could not save the transcript. The recording is kept."); update(); }
    });
}
void SpeechComposer::applyTranscript() {
    if (m_transcript.isEmpty() || !m_currentTarget().sameConversation(m_target)) return;
    const auto generation = m_generation;
    const auto id = m_id;
    const bool inserted = m_composer->insertVoiceText(m_draftTarget, m_transcript, [this, generation, id](bool saved) {
        if (generation != m_generation || id != m_id) return;
        if (!saved) { m_state = State::Preview; m_notice = tr("The transcript could not be saved. Add to draft retries the same insertion."); update(); return; }
        m_store.saveDurableAsync(storageKey(), {}, this, [this, generation, id](bool cleared, const ComposerDraft &) {
            if (generation != m_generation || id != m_id) return;
            if (!cleared) { m_state = State::Preview; m_notice = tr("The transcript is in the draft. Its recording receipt could not be cleared."); update(); return; }
            m_pcm.clear(); m_transcript.clear(); m_id.clear(); m_state = State::Idle; m_notice.clear(); update();
        });
    }, id);
    if (!inserted) { m_state = State::Preview; m_notice = tr("The draft changed during transcription. Add to draft inserts the saved transcript when you choose."); update(); }
}

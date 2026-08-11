import { useCallback, useEffect, useRef, useState } from 'react';
import { SpeechAPI, DEFAULT_REMOTE_URL } from './api/tauri-client';
import type { AppSettings, AsrLanguage, AutoCopyMode, Segment } from './api/tauri-client';
import { ControlPanel } from './components/ControlPanel';
import { SegmentCard } from './components/SegmentCard';
import { useAppStore } from './store/useAppStore';
import { Icon } from './components/ui/Icon';
import { isPermissionGranted, requestPermission, sendNotification } from '@tauri-apps/plugin-notification';
import { playCompletionSound } from './utils/notifySound';
import { wakeGate } from '../voice/wakeGate';
import { useVoiceConfig } from '../voice/useVoiceConfig';
import { useVoiceChannel } from '../voice/useVoiceChannel';
import { useToast } from '../voice/useToast';
import { ToastHost } from '../voice/Toast';
import { VoiceCommandPanel } from '../voice/VoiceCommandPanel';
import type { VoiceExchange, VoiceReply } from '../voice/types';

export default function SpeechPage() {
  // —— 语音指令通道 ——
  const toast = useToast();
  const { config: voiceConfig, ready: voiceReady, update: updateVoiceConfig } = useVoiceConfig();
  const [exchanges, setExchanges] = useState<VoiceExchange[]>([]);
  const lastExchangeIdRef = useRef<string | null>(null);

  const handleVoiceReply = useCallback((reply: VoiceReply) => {
    // 回复归到最近一次指令；无指令则单独成条。
    setExchanges((prev) => {
      const targetId = lastExchangeIdRef.current;
      if (targetId && prev.some((e) => e.id === targetId)) {
        return prev.map((e) =>
          e.id === targetId ? { ...e, replies: [...e.replies, reply] } : e,
        );
      }
      return prev;
    });
  }, []);

  const voiceChannel = useVoiceChannel({
    url: voiceConfig.url,
    sessionId: voiceConfig.sessionId,
    enabled: voiceReady,
    onReply: handleVoiceReply,
  });

  // 优化稿就绪 → 唤醒词门控 → 命中则 WS 送指令 + toast。
  const handleOptimizedText = useCallback(
    (text: string) => {
      const command = wakeGate(text, voiceConfig.wakeWords);
      if (!command) return;
      const ok = voiceChannel.send(command);
      if (ok) {
        const id = `ex-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
        lastExchangeIdRef.current = id;
        setExchanges((prev) => [
          ...prev,
          { id, command, replies: [], sentAt: Date.now() },
        ]);
        toast.push(`已发送指令：${command}`, 'success');
      } else {
        toast.push('未连接到 zero，指令未发送', 'error');
      }
    },
    [voiceConfig.wakeWords, voiceChannel, toast],
  );

  const store = useAppStore({ onOptimizedText: handleOptimizedText });
  const pollTimer = useRef<number | null>(null);
  const pollInFlightRef = useRef(false);
  const notifiedRevisionsRef = useRef<Set<string>>(new Set());
  const soundedRevisionsRef = useRef<Set<number>>(new Set());
  const notificationBaselineReadyRef = useRef(false);
  const [isBusy, setIsBusy] = useState(false);
  const [asrLanguage, setAsrLanguage] = useState<AsrLanguage>('zh');
  const [autoCopyMode, setAutoCopyMode] = useState<AutoCopyMode>('english');
  const [mergeWindowMs, setMergeWindowMs] = useState(3000);
  const [remoteUrl, setRemoteUrl] = useState<string>(DEFAULT_REMOTE_URL);
  const [remoteUrlPresets, setRemoteUrlPresets] = useState<string[]>([]);
  /** 「内置」项的地址：按 G10 局域网/外网设置派生，与录音实际连的地址同源。 */
  const [builtinRemoteUrl, setBuiltinRemoteUrl] = useState<string>(DEFAULT_REMOTE_URL);
  const [wantSecondary, setWantSecondary] = useState(false);
  const [notifySound, setNotifySound] = useState(true);
  const [autoPaste, setAutoPaste] = useState(false);
  const [autoPasteRewriteRetype, setAutoPasteRewriteRetype] = useState(false);
  const [autoStart, setAutoStart] = useState(false);
  const [captureEnabled, setCaptureEnabled] = useState(true);
  const [sceneLogEnabled, setSceneLogEnabled] = useState(true);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const autoStartFiredRef = useRef(false);

  // Load persisted settings from the local DB on mount.
  useEffect(() => {
    SpeechAPI.getSettings()
      .then((s) => {
        setAsrLanguage(s.asr_language);
        setAutoCopyMode(s.auto_copy_mode);
        setMergeWindowMs(s.merge_window_ms);
        setRemoteUrl(s.remote_url || DEFAULT_REMOTE_URL);
        setRemoteUrlPresets(s.remote_url_presets || []);
        setWantSecondary(!!s.want_secondary);
        setNotifySound(s.notify_sound !== false);
        setAutoPaste(!!s.auto_paste);
        setAutoPasteRewriteRetype(!!s.auto_paste_rewrite_retype);
        setAutoStart(!!s.auto_start);
        setCaptureEnabled(s.capture_enabled !== false);
        setSceneLogEnabled(s.scene_log_enabled !== false);
      })
      .catch((err) => console.warn('Load settings failed', err))
      .finally(() => setSettingsLoaded(true));
    // 内置项跟随 G10 网络设置（局域网 ws://…:8788 / 外网 wss://…:38788），失败则留常量。
    SpeechAPI.defaultRemoteUrl()
      .then((u) => u && setBuiltinRemoteUrl(u))
      .catch((err) => console.warn('Load default remote url failed', err));
  }, []);

  const showTranslationNotification = useCallback(async (segment: Segment) => {
    let permissionGranted = await isPermissionGranted();

    if (!permissionGranted) {
      const permission = await requestPermission();
      permissionGranted = permission === 'granted';
    }

    if (!permissionGranted) return;

    const body = segment.text_english?.trim() || '有新的翻译结果可查看';
    sendNotification({
      title: '识别完成',
      body: body.slice(0, 120),
    });
  }, []);

  useEffect(() => {
    isPermissionGranted().catch((err) => {
      console.error('Check notification permission failed', err);
    });
  }, []);

  const getSegmentKey = useCallback((seg: Segment) => {
    if (seg.segment_id !== null && seg.segment_id !== undefined) {
      return `seg-${seg.segment_id}`;
    }
    if (seg.id !== null && seg.id !== undefined) {
      return `db-${seg.id}`;
    }
    return `ts-${seg.start.toFixed(3)}`;
  }, []);

  const segmentsRef = useRef(store.segments);
  useEffect(() => {
    segmentsRef.current = store.segments;
  }, [store.segments]);

  // Recording Logic
  const stopPolling = useCallback(() => {
    if (pollTimer.current) {
      clearInterval(pollTimer.current);
      pollTimer.current = null;
    }
  }, []);

  const startPolling = useCallback(() => {
    if (pollTimer.current) clearInterval(pollTimer.current);
    pollTimer.current = window.setInterval(async () => {
      if (pollInFlightRef.current) return;
      pollInFlightRef.current = true;
      try {
        const init = await SpeechAPI.getInitStatus();
        if (init.status === 2) {
          store.setErrorMessage(init.error || '无法连接识别服务');
          store.setStatus('error');
          stopPolling();
          return;
        }

        const state = await SpeechAPI.getRecordingState();
        const hasPending = segmentsRef.current.some(
          (seg) =>
            seg.optimize_status === 'pending' ||
            seg.optimize_status === 'running' ||
            seg.translate_status === 'pending' ||
            seg.translate_status === 'running'
        );
        if (state.recording) {
          store.setStatus(hasPending ? 'processing' : 'recording');
          return;
        }

        if (hasPending) {
          store.setStatus('processing');
          return;
        }

        stopPolling();
        store.setStatus('finished');
      } catch (err) {
        console.error("Poll failed", err);
        stopPolling();
        store.setStatus('error');
      } finally {
        pollInFlightRef.current = false;
      }
    }, 1000);
  }, [store, stopPolling]);

  useEffect(() => () => stopPolling(), [stopPolling]);

  useEffect(() => {
    if (!notificationBaselineReadyRef.current) {
      store.segments.forEach((seg) => {
        if (seg.revision !== undefined) {
          const optKey = `opt-${seg.revision}`;
          const transKey = `trans-${seg.revision}`;
          if (seg.optimize_status === 'success') notifiedRevisionsRef.current.add(optKey);
          if (seg.translate_status === 'success') notifiedRevisionsRef.current.add(transKey);
          // 挂载时已完成的段不该补响提示音。
          if (seg.optimize_status === 'success' && seg.translate_status === 'success') {
            soundedRevisionsRef.current.add(seg.revision);
          }
        }
      });
      notificationBaselineReadyRef.current = true;
      return;
    }

    store.segments.forEach((segment) => {
      const revision = segment.revision;
      if (revision === undefined) return;

      const optKey = `opt-${revision}`;
      if (segment.optimize_status === 'success' && !notifiedRevisionsRef.current.has(optKey)) {
        notifiedRevisionsRef.current.add(optKey);
        showTranslationNotification(segment).catch(console.error);
      }

      const transKey = `trans-${revision}`;
      if (segment.translate_status === 'success' && !notifiedRevisionsRef.current.has(transKey)) {
        notifiedRevisionsRef.current.add(transKey);
        showTranslationNotification(segment).catch(console.error);
      }

      // 转写 + 优化 + 翻译都成功后，每段响一次完成提示音（受「完成提示音」开关控制）。
      if (
        segment.optimize_status === 'success' &&
        segment.translate_status === 'success' &&
        !soundedRevisionsRef.current.has(revision)
      ) {
        soundedRevisionsRef.current.add(revision);
        if (notifySound) playCompletionSound();
      }
    });
  }, [showTranslationNotification, store.segments, notifySound]);

  // Sync initial recording state on mount
  useEffect(() => {
    if (!store.isInitialized) return;

    const sync = async () => {
      try {
        const state = await SpeechAPI.getRecordingState();
        if (state.recording) {
          startPolling();
        }
      } catch (err) {
        console.error("Initial sync recording state failed", err);
      }
    };
    sync();
  }, [store.isInitialized, startPolling]);

  const startRecording = async () => {
    try {
      setIsBusy(true);
      store.setErrorMessage('');
      await SpeechAPI.startRecording();
      store.setStatus('recording');
      startPolling();
    } catch (err) {
      console.error("Start failed", err);
      store.setErrorMessage(typeof err === 'string' ? err : (err as Error)?.message || String(err));
      store.setStatus('error');
    } finally {
      setIsBusy(false);
    }
  };

  // 启动时自动开始识别：设置加载完 + 设备就绪 + 开关打开 + 未在录音时触发，仅一次。
  useEffect(() => {
    if (autoStartFiredRef.current) return;
    if (!settingsLoaded || !store.isInitialized || !autoStart) return;
    if (store.devices.length === 0) return;
    autoStartFiredRef.current = true;
    (async () => {
      try {
        const st = await SpeechAPI.getRecordingState();
        if (st.recording) return;
        await startRecording();
      } catch (err) {
        console.error('auto start recording failed', err);
      }
    })();
    // startRecording 为每次渲染重建的普通函数，靠 autoStartFiredRef 保证只触发一次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settingsLoaded, store.isInitialized, autoStart, store.devices.length]);

  const retryRecording = () => {
    store.setErrorMessage('');
    store.setStatus('idle');
    startRecording();
  };

  const stopRecording = async () => {
    try {
      setIsBusy(true);
      store.setStatus('processing');
      await SpeechAPI.stopRecording();
    } catch (err) {
      console.error("Stop failed", err);
      store.setStatus('error');
    } finally {
      setIsBusy(false);
    }
  };

  const waitUntilNotRecording = useCallback(async (timeoutMs = 4000): Promise<boolean> => {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      try {
        const s = await SpeechAPI.getRecordingState();
        if (!s.recording) return true;
      } catch (err) {
        console.warn('waitUntilNotRecording probe failed', err);
      }
      await new Promise((r) => setTimeout(r, 150));
    }
    return false;
  }, []);

  const persistAndMaybeReconnect = useCallback(
    async (next: AppSettings, urlChanged: boolean) => {
      try {
        await SpeechAPI.applySettings(next);
      } catch (err) {
        console.error('apply settings failed', err);
        return;
      }
      if (!urlChanged) return;
      let wasRecording = false;
      try {
        wasRecording = (await SpeechAPI.getRecordingState()).recording;
      } catch (err) {
        console.warn('probe recording state failed', err);
      }
      if (!wasRecording) return;
      try {
        await SpeechAPI.stopRecording();
      } catch (err) {
        console.error('stop for reconnect failed', err);
      }
      const settled = await waitUntilNotRecording();
      if (!settled) {
        console.warn('reconnect: previous session did not stop in time');
      }
      await startRecording();
    },
    [waitUntilNotRecording]
  );

  const handleRemoteUrlSelect = (url: string) => {
    if (url === remoteUrl) return;
    setRemoteUrl(url);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: url,
      remote_url_presets: remoteUrlPresets,
      want_secondary: wantSecondary,
      notify_sound: notifySound,
      auto_paste: autoPaste,
      auto_paste_rewrite_retype: autoPasteRewriteRetype,
      auto_start: autoStart,
      capture_enabled: captureEnabled,
      scene_log_enabled: sceneLogEnabled,
    };
    persistAndMaybeReconnect(next, true);
  };

  const handleRemoteUrlAdd = (url: string) => {
    const trimmed = url.trim();
    if (!trimmed) return;
    const presetsNext = remoteUrlPresets.includes(trimmed)
      ? remoteUrlPresets
      : [...remoteUrlPresets, trimmed];
    const urlChanged = trimmed !== remoteUrl;
    setRemoteUrlPresets(presetsNext);
    setRemoteUrl(trimmed);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: trimmed,
      remote_url_presets: presetsNext,
      want_secondary: wantSecondary,
      notify_sound: notifySound,
      auto_paste: autoPaste,
      auto_paste_rewrite_retype: autoPasteRewriteRetype,
      auto_start: autoStart,
      capture_enabled: captureEnabled,
      scene_log_enabled: sceneLogEnabled,
    };
    persistAndMaybeReconnect(next, urlChanged);
  };

  const handleNotifySoundChange = (val: boolean) => {
    if (val === notifySound) return;
    setNotifySound(val);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: remoteUrl,
      remote_url_presets: remoteUrlPresets,
      want_secondary: wantSecondary,
      notify_sound: val,
      auto_paste: autoPaste,
      auto_paste_rewrite_retype: autoPasteRewriteRetype,
      auto_start: autoStart,
      capture_enabled: captureEnabled,
      scene_log_enabled: sceneLogEnabled,
    };
    SpeechAPI.applySettings(next).catch((err) =>
      console.error('apply notify_sound failed', err)
    );
  };

  const handleAutoPasteChange = (val: boolean) => {
    if (val === autoPaste) return;
    setAutoPaste(val);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: remoteUrl,
      remote_url_presets: remoteUrlPresets,
      want_secondary: wantSecondary,
      notify_sound: notifySound,
      auto_paste: val,
      auto_paste_rewrite_retype: autoPasteRewriteRetype,
      auto_start: autoStart,
      capture_enabled: captureEnabled,
      scene_log_enabled: sceneLogEnabled,
    };
    SpeechAPI.applySettings(next).catch((err) =>
      console.error('apply auto_paste failed', err)
    );
  };

  const handleAutoPasteRewriteRetypeChange = (val: boolean) => {
    if (val === autoPasteRewriteRetype) return;
    setAutoPasteRewriteRetype(val);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: remoteUrl,
      remote_url_presets: remoteUrlPresets,
      want_secondary: wantSecondary,
      notify_sound: notifySound,
      auto_paste: autoPaste,
      auto_paste_rewrite_retype: val,
      auto_start: autoStart,
      capture_enabled: captureEnabled,
      scene_log_enabled: sceneLogEnabled,
    };
    SpeechAPI.applySettings(next).catch((err) =>
      console.error('apply auto_paste_rewrite_retype failed', err)
    );
  };

  const handleAutoStartChange = (val: boolean) => {
    if (val === autoStart) return;
    setAutoStart(val);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: remoteUrl,
      remote_url_presets: remoteUrlPresets,
      want_secondary: wantSecondary,
      notify_sound: notifySound,
      auto_paste: autoPaste,
      auto_paste_rewrite_retype: autoPasteRewriteRetype,
      auto_start: val,
      capture_enabled: captureEnabled,
      scene_log_enabled: sceneLogEnabled,
    };
    SpeechAPI.applySettings(next).catch((err) =>
      console.error('apply auto_start failed', err)
    );
  };

  const handleCaptureEnabledChange = (val: boolean) => {
    if (val === captureEnabled) return;
    setCaptureEnabled(val);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: remoteUrl,
      remote_url_presets: remoteUrlPresets,
      want_secondary: wantSecondary,
      notify_sound: notifySound,
      auto_paste: autoPaste,
      auto_paste_rewrite_retype: autoPasteRewriteRetype,
      auto_start: autoStart,
      capture_enabled: val,
      scene_log_enabled: sceneLogEnabled,
    };
    SpeechAPI.applySettings(next).catch((err) =>
      console.error('apply capture_enabled failed', err)
    );
  };

  const handleSceneLogEnabledChange = (val: boolean) => {
    if (val === sceneLogEnabled) return;
    setSceneLogEnabled(val);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: remoteUrl,
      remote_url_presets: remoteUrlPresets,
      want_secondary: wantSecondary,
      notify_sound: notifySound,
      auto_paste: autoPaste,
      auto_paste_rewrite_retype: autoPasteRewriteRetype,
      auto_start: autoStart,
      capture_enabled: captureEnabled,
      scene_log_enabled: val,
    };
    SpeechAPI.applySettings(next).catch((err) =>
      console.error('apply scene_log_enabled failed', err)
    );
  };

  const handleWantSecondaryChange = (val: boolean) => {
    if (val === wantSecondary) return;
    setWantSecondary(val);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: remoteUrl,
      remote_url_presets: remoteUrlPresets,
      want_secondary: val,
      notify_sound: notifySound,
      auto_paste: autoPaste,
      auto_paste_rewrite_retype: autoPasteRewriteRetype,
      auto_start: autoStart,
      capture_enabled: captureEnabled,
      scene_log_enabled: sceneLogEnabled,
    };
    persistAndMaybeReconnect(next, true);
  };

  const handleRemoteUrlRemove = (url: string) => {
    const presetsNext = remoteUrlPresets.filter((p) => p !== url);
    // 删掉的正是当前选中项 → 回落到「内置」（跟随 G10 设置的派生地址）。
    const fallback = remoteUrl === url ? builtinRemoteUrl : remoteUrl;
    const urlChanged = fallback !== remoteUrl;
    setRemoteUrlPresets(presetsNext);
    setRemoteUrl(fallback);
    const next: AppSettings = {
      asr_language: asrLanguage,
      auto_copy_mode: autoCopyMode,
      merge_window_ms: mergeWindowMs,
      remote_url: fallback,
      remote_url_presets: presetsNext,
      want_secondary: wantSecondary,
      notify_sound: notifySound,
      auto_paste: autoPaste,
      auto_paste_rewrite_retype: autoPasteRewriteRetype,
      auto_start: autoStart,
      capture_enabled: captureEnabled,
      scene_log_enabled: sceneLogEnabled,
    };
    persistAndMaybeReconnect(next, urlChanged);
  };

  const handleCopy = async (text: string) => {
    await SpeechAPI.copyToClipboard(text);
  };

  const handleSegmentCopy = (text: string) => {
    handleCopy(text).catch((err) => console.error('Copy segment text failed', err));
  };

  const handleClear = async () => {
    await SpeechAPI.clearResults();
    store.setSegments([]);
  };

  const handleDeviceChange = async (device: string) => {
    store.setSelectedDevice(device);
    try {
      await SpeechAPI.setInputDevice(device);
    } catch (err) {
      console.error("Set input device failed", err);
      store.setStatus('error');
    }
  };

  const [exporting, setExporting] = useState(false);
  const [exportingHomophone, setExportingHomophone] = useState(false);
  const [exportingHotword, setExportingHotword] = useState(false);

  const handleExportSamples = async () => {
    setExporting(true);
    try {
      const path = await SpeechAPI.exportSamples();
      await SpeechAPI.openInFolder(path);
    } catch (err) {
      console.error('export samples failed', err);
      store.setErrorMessage(typeof err === 'string' ? err : (err as Error)?.message || String(err));
    } finally {
      setExporting(false);
    }
  };

  // P2a：对纠错样本跑同音候选挖掘，导出 JSON 供人工审读（不建表、不改配置）。
  const handleExportHomophone = async () => {
    setExportingHomophone(true);
    try {
      const path = await SpeechAPI.exportHomophoneCandidates();
      await SpeechAPI.openInFolder(path);
    } catch (err) {
      console.error('export homophone candidates failed', err);
      store.setErrorMessage(typeof err === 'string' ? err : (err as Error)?.message || String(err));
    } finally {
      setExportingHomophone(false);
    }
  };

  // P1.5：两路挖热词候选（纠错样本 + 场景记录），导出 JSON 供人工审读（不改任何配置）。
  const handleExportHotword = async () => {
    setExportingHotword(true);
    try {
      const path = await SpeechAPI.exportHotwordCandidates();
      await SpeechAPI.openInFolder(path);
    } catch (err) {
      console.error('export hotword candidates failed', err);
      store.setErrorMessage(typeof err === 'string' ? err : (err as Error)?.message || String(err));
    } finally {
      setExportingHotword(false);
    }
  };

  const displaySegments = store.segments.slice().reverse();

  return (
    <div className="flex h-full overflow-hidden bg-[var(--bg-canvas)]">
      <div className="shrink-0 h-full">
        <ControlPanel
          status={store.status}
          devices={store.devices}
          selectedDevice={store.selectedDevice}
          onDeviceChange={handleDeviceChange}
          showEnglish={store.showEnglish}
          onShowEnglishChange={store.setShowEnglish}
          onStart={startRecording}
          onStop={stopRecording}
          onRetry={retryRecording}
          errorMessage={store.errorMessage}
          onClear={handleClear}
          asrLanguage={asrLanguage}
          onAsrLanguageChange={(v) => {
            setAsrLanguage(v);
            const next: AppSettings = {
              asr_language: v,
              auto_copy_mode: autoCopyMode,
              merge_window_ms: mergeWindowMs,
              remote_url: remoteUrl,
              remote_url_presets: remoteUrlPresets,
              want_secondary: wantSecondary,
              notify_sound: notifySound,
              auto_paste: autoPaste,
              auto_paste_rewrite_retype: autoPasteRewriteRetype,
              auto_start: autoStart,
              capture_enabled: captureEnabled,
              scene_log_enabled: sceneLogEnabled,
            };
            SpeechAPI.applySettings(next).catch((err) => console.error('apply asr_language failed', err));
          }}
          autoCopyMode={autoCopyMode}
          onAutoCopyModeChange={(v) => {
            setAutoCopyMode(v);
            const next: AppSettings = {
              asr_language: asrLanguage,
              auto_copy_mode: v,
              merge_window_ms: mergeWindowMs,
              remote_url: remoteUrl,
              remote_url_presets: remoteUrlPresets,
              want_secondary: wantSecondary,
              notify_sound: notifySound,
              auto_paste: autoPaste,
              auto_paste_rewrite_retype: autoPasteRewriteRetype,
              auto_start: autoStart,
              capture_enabled: captureEnabled,
              scene_log_enabled: sceneLogEnabled,
            };
            SpeechAPI.applySettings(next).catch((err) => console.error('apply auto_copy_mode failed', err));
          }}
          autoPaste={autoPaste}
          onAutoPasteChange={handleAutoPasteChange}
          autoPasteRewriteRetype={autoPasteRewriteRetype}
          onAutoPasteRewriteRetypeChange={handleAutoPasteRewriteRetypeChange}
          autoStart={autoStart}
          onAutoStartChange={handleAutoStartChange}
          mergeWindowMs={mergeWindowMs}
          onMergeWindowMsChange={(v) => {
            setMergeWindowMs(v);
            const next: AppSettings = {
              asr_language: asrLanguage,
              auto_copy_mode: autoCopyMode,
              merge_window_ms: v,
              remote_url: remoteUrl,
              remote_url_presets: remoteUrlPresets,
              want_secondary: wantSecondary,
              notify_sound: notifySound,
              auto_paste: autoPaste,
              auto_paste_rewrite_retype: autoPasteRewriteRetype,
              auto_start: autoStart,
              capture_enabled: captureEnabled,
              scene_log_enabled: sceneLogEnabled,
            };
            SpeechAPI.applySettings(next).catch((err) => console.error('apply merge_window_ms failed', err));
          }}
          remoteUrl={remoteUrl}
          remoteUrlPresets={remoteUrlPresets}
          builtinRemoteUrl={builtinRemoteUrl}
          onRemoteUrlSelect={handleRemoteUrlSelect}
          onRemoteUrlAdd={handleRemoteUrlAdd}
          onRemoteUrlRemove={handleRemoteUrlRemove}
          wantSecondary={wantSecondary}
          onWantSecondaryChange={handleWantSecondaryChange}
          notifySound={notifySound}
          onNotifySoundChange={handleNotifySoundChange}
          captureEnabled={captureEnabled}
          onCaptureEnabledChange={handleCaptureEnabledChange}
          sceneLogEnabled={sceneLogEnabled}
          onSceneLogEnabledChange={handleSceneLogEnabledChange}
          disabled={isBusy}
        />
      </div>

      <main className="flex-1 flex flex-col min-w-0">
        <div className="flex-1 overflow-y-auto p-4 px-6">
          <div className="max-w-none mx-0 flex flex-col gap-3">
            <VoiceCommandPanel
              state={voiceChannel.state}
              config={voiceConfig}
              exchanges={exchanges}
              onUpdateConfig={updateVoiceConfig}
            />

            <div className="flex justify-end gap-1">
              <button
                onClick={handleExportHotword}
                disabled={exportingHotword}
                className="inline-flex items-center gap-1.5 h-7 px-2.5 text-[11px] rounded-md text-[var(--ink-4)] hover:text-[var(--ink-2)] hover:bg-[var(--bg-soft)] transition-colors disabled:opacity-50"
                title="从纠错样本与场景记录挖掘热词候选，导出 JSON 供人工审读（热词是提示不是替换，无过纠风险）"
              >
                <Icon name={exportingHotword ? 'refresh' : 'wand'} size={12} className={exportingHotword ? 'animate-spin' : undefined} />
                {exportingHotword ? '挖掘中...' : '导出热词候选'}
              </button>
              <button
                onClick={handleExportHomophone}
                disabled={exportingHomophone}
                className="inline-flex items-center gap-1.5 h-7 px-2.5 text-[11px] rounded-md text-[var(--ink-4)] hover:text-[var(--ink-2)] hover:bg-[var(--bg-soft)] transition-colors disabled:opacity-50"
                title="对纠错样本挖掘同音替换候选，导出 JSON 供人工审读（exact 级才可考虑入表）"
              >
                <Icon name={exportingHomophone ? 'refresh' : 'wand'} size={12} className={exportingHomophone ? 'animate-spin' : undefined} />
                {exportingHomophone ? '挖掘中...' : '导出同音候选'}
              </button>
              <button
                onClick={handleExportSamples}
                disabled={exporting}
                className="inline-flex items-center gap-1.5 h-7 px-2.5 text-[11px] rounded-md text-[var(--ink-4)] hover:text-[var(--ink-2)] hover:bg-[var(--bg-soft)] transition-colors disabled:opacity-50"
                title="导出全部标注样本为 JSON 并打开所在文件夹"
              >
                <Icon name={exporting ? 'refresh' : 'download'} size={12} className={exporting ? 'animate-spin' : undefined} />
                {exporting ? '导出中...' : '导出标注样本'}
              </button>
            </div>

            {store.segments.length === 0 && (store.status === 'idle' || store.status === 'finished') && (
              <div className="flex flex-col items-center justify-center py-40 gap-4 opacity-30">
                <Icon name="mic" size={48} stroke={1.2} />
                <p className="text-sm font-medium">
                  {store.status === 'idle' ? '准备就绪，点击"开始识别"开始识别' : '当前没有可展示的识别结果'}
                </p>
              </div>
            )}

            {displaySegments.map((seg) => (
              <SegmentCard
                key={getSegmentKey(seg)}
                segment={seg}
                showEnglish={store.showEnglish}
                onCopyChinese={(text) => handleSegmentCopy(text)}
                onCopyEnglish={(text) => handleSegmentCopy(text)}
              />
            ))}
          </div>
        </div>
      </main>

      <ToastHost toasts={toast.toasts} onDismiss={toast.dismiss} />
    </div>
  );
}

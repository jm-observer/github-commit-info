import React from 'react';
import { RecordCard } from './RecordCard';
import { Dropdown } from './ui/Dropdown';
import { NumberField } from './ui/NumberField';
import { Switch } from './ui/Switch';
import { Button } from './ui/Button';
import { Icon } from './ui/Icon';
import { StatusChip } from './StatusChip';
import { RemoteUrlPicker } from './RemoteUrlPicker';
import { SceneStatsCard } from './SceneStatsCard';
import type { AsrLanguage, AutoCopyMode } from '../api/tauri-client';

interface ControlPanelProps {
  status: string;
  devices: { label: string; value: string }[];
  selectedDevice: string;
  onDeviceChange: (val: string) => void;
  showEnglish: boolean;
  onShowEnglishChange: (val: boolean) => void;
  onStart: () => void;
  onStop: () => void;
  onRetry: () => void;
  errorMessage: string;
  onClear: () => void;
  asrLanguage: AsrLanguage;
  onAsrLanguageChange: (val: AsrLanguage) => void;
  autoCopyMode: AutoCopyMode;
  onAutoCopyModeChange: (val: AutoCopyMode) => void;
  autoPaste: boolean;
  onAutoPasteChange: (val: boolean) => void;
  autoPasteRewriteRetype: boolean;
  onAutoPasteRewriteRetypeChange: (val: boolean) => void;
  autoStart: boolean;
  onAutoStartChange: (val: boolean) => void;
  mergeWindowMs: number;
  onMergeWindowMsChange: (val: number) => void;
  remoteUrl: string;
  remoteUrlPresets: string[];
  onRemoteUrlSelect: (url: string) => void;
  onRemoteUrlAdd: (url: string) => void;
  onRemoteUrlRemove: (url: string) => void;
  wantSecondary: boolean;
  onWantSecondaryChange: (val: boolean) => void;
  notifySound: boolean;
  onNotifySoundChange: (val: boolean) => void;
  captureEnabled: boolean;
  onCaptureEnabledChange: (val: boolean) => void;
  disabled?: boolean;
}

/** Bounds for the auto-copy stitch window input (seconds). */
const MERGE_WINDOW_MIN_S = 0;
const MERGE_WINDOW_MAX_S = 60;

const LANGUAGE_OPTIONS: { label: string; value: AsrLanguage }[] = [
  { label: '自动检测', value: '' },
  { label: '中文', value: 'zh' },
  { label: '英文', value: 'en' },
  { label: '日文', value: 'ja' },
  { label: '韩文', value: 'ko' },
  { label: '粤语', value: 'yue' },
];

const AUTO_COPY_OPTIONS: { label: string; value: AutoCopyMode }[] = [
  { label: '关闭', value: 'off' },
  { label: '复制中文优化', value: 'optimized_zh' },
  { label: '复制英文翻译', value: 'english' },
];

export const ControlPanel: React.FC<ControlPanelProps> = ({
  status,
  devices,
  selectedDevice,
  onDeviceChange,
  showEnglish,
  onShowEnglishChange,
  onStart,
  onStop,
  onRetry,
  errorMessage,
  onClear,
  asrLanguage,
  onAsrLanguageChange,
  autoCopyMode,
  onAutoCopyModeChange,
  autoPaste,
  onAutoPasteChange,
  autoPasteRewriteRetype,
  onAutoPasteRewriteRetypeChange,
  autoStart,
  onAutoStartChange,
  mergeWindowMs,
  onMergeWindowMsChange,
  remoteUrl,
  remoteUrlPresets,
  onRemoteUrlSelect,
  onRemoteUrlAdd,
  onRemoteUrlRemove,
  wantSecondary,
  onWantSecondaryChange,
  notifySound,
  onNotifySoundChange,
  captureEnabled,
  onCaptureEnabledChange,
  disabled,
}) => {
  return (
    <aside className="w-80 shrink-0 h-[calc(100%-24px)] my-3 mx-3 flex flex-col bg-[var(--bg-app)] border border-[var(--line)] rounded-[16px] px-6 py-4 gap-4 overflow-y-auto">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2.5">
          <div className="w-8 h-8 rounded-[9px] bg-gradient-to-br from-[var(--primary)] to-[var(--primary-deep)] shadow-sm flex items-center justify-center text-white">
            <Icon name="logo" size={20} stroke={2} />
          </div>
          <div className="flex flex-col">
            <h1 className="text-[14.5px] font-semibold text-[var(--ink)] leading-tight">StreamSpeech</h1>
            <span className="text-[11px] text-[var(--ink-4)]">远程语音识别</span>
          </div>
        </div>
      </div>

      <StatusChip status={status} />

      <RemoteUrlPicker
        value={remoteUrl}
        presets={remoteUrlPresets}
        onSelect={onRemoteUrlSelect}
        onAdd={onRemoteUrlAdd}
        onRemove={onRemoteUrlRemove}
        disabled={disabled}
      />

      <Dropdown
        label="输入设备"
        icon="mic"
        options={devices}
        value={selectedDevice}
        onChange={onDeviceChange}
        disabled={status === 'recording'}
      />

      <Dropdown
        label="识别语言"
        icon="languages"
        options={LANGUAGE_OPTIONS}
        value={asrLanguage}
        onChange={(v) => onAsrLanguageChange(v as AsrLanguage)}
        disabled={status === 'recording' || disabled}
      />

      <Dropdown
        label="自动复制"
        icon="copy"
        options={AUTO_COPY_OPTIONS}
        value={autoCopyMode}
        onChange={(v) => onAutoCopyModeChange(v as AutoCopyMode)}
        disabled={disabled}
      />

      <div>
        <NumberField
          label="合并间隔"
          icon="clock"
          value={mergeWindowMs / 1000}
          onChange={(sec) => onMergeWindowMsChange(Math.round(sec * 1000))}
          min={MERGE_WINDOW_MIN_S}
          max={MERGE_WINDOW_MAX_S}
          step={0.5}
          suffix="秒"
          disabled={disabled || autoCopyMode === 'off'}
        />
        <p className="mt-1.5 ml-1 text-[11px] leading-relaxed text-[var(--ink-4)]">
          相邻句子间隔在此时间内时，自动复制会拼接为一次粘贴；设为 0 则关闭合并。
        </p>
      </div>

      <div className="flex items-center justify-between">
        <div className="flex flex-col">
          <span className="text-[13px] font-medium text-[var(--ink-2)]">自动粘贴到焦点</span>
          <span className="text-[11px] text-[var(--ink-4)]">
            识别完成后逐段直接输入到当前光标处（按上方“自动复制”选中的内容）；焦点不在输入框时不打扰，仍可回头 Ctrl+V
          </span>
        </div>
        <Switch
          checked={autoPaste}
          onCheckedChange={onAutoPasteChange}
          disabled={disabled || autoCopyMode === 'off'}
        />
      </div>

      <div className="flex items-center justify-between">
        <div className="flex flex-col pr-3">
          <span className="text-[13px] font-medium text-[var(--ink-2)]">改写时回退重打</span>
          <span className="text-[11px] text-[var(--ink-4)]">
            同段优化稿被 LLM 改写时，按公共前缀长度发退格回退已输入字符再补打新尾巴。关闭则保守跳过，剩余内容留给剪贴板（Ctrl+V）兜底。开启更跟手，但焦点若已切走会误删别处内容
          </span>
        </div>
        <Switch
          checked={autoPasteRewriteRetype}
          onCheckedChange={onAutoPasteRewriteRetypeChange}
          disabled={disabled || autoCopyMode === 'off' || !autoPaste}
        />
      </div>

      <RecordCard
        status={status}
        onStart={onStart}
        onStop={onStop}
        onRetry={onRetry}
        errorMessage={errorMessage}
        disabled={devices.length === 0 || disabled}
      />

      <div className="flex flex-col gap-4 mt-1">
        <div className="flex items-center justify-between">
          <div className="flex flex-col">
            <span className="text-[13px] font-medium text-[var(--ink-2)]">启动时自动识别</span>
            <span className="text-[11px] text-[var(--ink-4)]">打开本页/程序启动且设备就绪后，自动开始录音识别，无需手动点开始</span>
          </div>
          <Switch checked={autoStart} onCheckedChange={onAutoStartChange} disabled={disabled} />
        </div>
        <div className="flex items-center justify-between">
          <div className="flex flex-col">
            <span className="text-[13px] font-medium text-[var(--ink-2)]">显示英文翻译</span>
            <span className="text-[11px] text-[var(--ink-4)]">LLM 同步生成对照翻译</span>
          </div>
          <Switch checked={showEnglish} onCheckedChange={onShowEnglishChange} />
        </div>
        <div className="flex items-center justify-between">
          <div className="flex flex-col">
            <span className="text-[13px] font-medium text-[var(--ink-2)]">次模型对比识别</span>
            <span className="text-[11px] text-[var(--ink-4)]">同段并行跑次模型(中文),在原文下方对照展示。变更需停-启录音生效</span>
          </div>
          <Switch checked={wantSecondary} onCheckedChange={onWantSecondaryChange} disabled={disabled} />
        </div>
        <div className="flex items-center justify-between">
          <div className="flex flex-col">
            <span className="text-[13px] font-medium text-[var(--ink-2)]">完成提示音</span>
            <span className="text-[11px] text-[var(--ink-4)]">每段优化+翻译完成时伴随托盘图标跳动播放短促系统音；扬声器+麦克风录音时建议关闭以避免被采入</span>
          </div>
          <Switch checked={notifySound} onCheckedChange={onNotifySoundChange} />
        </div>
        <div className="flex items-center justify-between">
          <div className="flex flex-col pr-3">
            <span className="text-[13px] font-medium text-[var(--ink-2)]">启用纠错一键采集</span>
            <span className="text-[11px] text-[var(--ink-4)]">
              改好优化稿并复制整段后按 Ctrl+Alt+X，读取剪贴板与最近交付比对并落库，用于积累纠错语料
            </span>
          </div>
          <Switch checked={captureEnabled} onCheckedChange={onCaptureEnabledChange} disabled={disabled} />
        </div>
        {/* 日常全量收集的实时读数，与上面「手动采集」的开关并列但不是一回事，见组件文档。 */}
        <SceneStatsCard />
      </div>

      <div className="flex-1" />

      <div className="pt-4 border-t border-[var(--line)]">
        <Button variant="soft" size="sm" className="w-full h-9 rounded-lg text-xs" onClick={onClear} disabled={status === 'recording' || disabled}>
          清空结果
        </Button>
        <p className="mt-3 text-[11px] leading-relaxed text-[var(--ink-4)]">
          ASR 模型、LLM 提示词、声纹等服务端配置请打开
          <br />
          GB10 管理台 (http://&lt;server&gt;:8090/) 调整。
        </p>
      </div>
    </aside>
  );
};

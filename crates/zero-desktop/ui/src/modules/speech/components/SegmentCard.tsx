import React, { useEffect, useRef, useState } from 'react';
import { cn, stripYear } from '../utils';
import type { Sample, SampleLabel, Segment } from '../api/tauri-client';
import { SpeechAPI } from '../api/tauri-client';
import { Button } from './ui/Button';
import { Icon } from './ui/Icon';
import { Dropdown } from './ui/Dropdown';
import { Switch } from './ui/Switch';

const LABEL_OPTIONS: { label: string; value: SampleLabel }[] = [
  { label: '识别错误', value: 'asr_wrong' },
  // 与「识别错误」正交：那个是文字识别错，这个是说话人认错（文字可能完全正确）。
  { label: '声纹识别错误', value: 'speaker_wrong' },
  { label: '热词纠错', value: 'hotword' },
  { label: '优化不当', value: 'bad_optimize' },
  { label: '正常无需过滤', value: 'ok' },
  { label: '其它', value: 'other' },
];

const AUDIO_STATUS_TEXT: Record<string, string> = {
  saved: '音频已存档',
  expired: '音频已过期',
  fetch_failed: '音频拉取失败',
  skipped: '未存档音频',
};

const HOTWORD_SYNC_TEXT: Record<string, string> = {
  added: '已加入热词表',
  exists: '热词已存在',
  failed: '热词同步失败',
};

interface SegmentCardProps {
  segment: Segment;
  showEnglish?: boolean;
  /** When the dual-model comparison opt-in is on, show the secondary
   *  recognizer's text in a small accent row. Defaults to true so any
   *  segment carrying `text_secondary` is shown — toggling the feature off
   *  for new sessions naturally hides it for future segments. */
  showSecondary?: boolean;
  onCopyChinese: (text: string) => void;
  onCopyEnglish: (text: string) => void;
}

export const SegmentCard: React.FC<SegmentCardProps> = ({
  segment,
  showEnglish,
  showSecondary = true,
  onCopyChinese,
  onCopyEnglish,
}) => {
  const [copiedZh, setCopiedZh] = useState(false);
  const [copiedEn, setCopiedEn] = useState(false);
  const [copiedSec, setCopiedSec] = useState(false);
  // 标注面板本会话内状态。
  const [panelOpen, setPanelOpen] = useState(false);
  const [label, setLabel] = useState<SampleLabel>('asr_wrong');
  const [correction, setCorrection] = useState('');
  const [note, setNote] = useState('');
  const [syncHotword, setSyncHotword] = useState(true);
  const [saving, setSaving] = useState(false);
  const [markError, setMarkError] = useState('');
  const [marked, setMarked] = useState(false);
  const [markResult, setMarkResult] = useState<Sample | null>(null);
  // 试听该段原始音频（判断到底是没说清还是识别错了）。音频在服务端只保留 1 天。
  const [audioState, setAudioState] = useState<'idle' | 'loading' | 'playing'>('idle');
  const [audioError, setAudioError] = useState('');
  const audioRef = useRef<HTMLAudioElement | null>(null);
  // base64 结果按卡片缓存，重播不重拉。
  const audioB64Ref = useRef<string | null>(null);

  // 卡片卸载时停掉还在播的音频。
  useEffect(
    () => () => {
      audioRef.current?.pause();
      audioRef.current = null;
    },
    []
  );

  const handleAudition = async () => {
    if (audioState === 'playing') {
      audioRef.current?.pause();
      audioRef.current = null;
      setAudioState('idle');
      return;
    }
    if (audioState === 'loading') return;
    setAudioError('');
    try {
      let b64 = audioB64Ref.current;
      if (!b64) {
        setAudioState('loading');
        const segId = (segment.segment_id ?? segment.id) ?? 0;
        b64 = await SpeechAPI.fetchSegmentAudio(segId);
        audioB64Ref.current = b64;
      }
      const audio = new Audio(`data:audio/wav;base64,${b64}`);
      audioRef.current = audio;
      audio.onended = () => {
        audioRef.current = null;
        setAudioState('idle');
      };
      audio.onerror = () => {
        audioRef.current = null;
        setAudioState('idle');
        setAudioError('播放失败');
      };
      await audio.play();
      setAudioState('playing');
    } catch (err) {
      setAudioState('idle');
      setAudioError(typeof err === 'string' ? err : (err as Error)?.message || String(err));
    }
  };

  const handleCopyZh = () => {
    onCopyChinese(segment.text_optimized || segment.text_raw);
    setCopiedZh(true);
    setTimeout(() => setCopiedZh(false), 2000);
  };

  const handleCopyEn = () => {
    onCopyEnglish(segment.text_english || '');
    setCopiedEn(true);
    setTimeout(() => setCopiedEn(false), 2000);
  };

  const handleCopySec = () => {
    if (!segment.text_secondary) return;
    onCopyChinese(segment.text_secondary);
    setCopiedSec(true);
    setTimeout(() => setCopiedSec(false), 2000);
  };

  // 打开标注面板：按当前标签预填内容。
  const openPanel = () => {
    if (label === 'asr_wrong') {
      setCorrection(segment.text_raw || '');
    } else if (label === 'bad_optimize') {
      setCorrection(segment.text_optimized || '');
    }
    setMarkError('');
    setPanelOpen(true);
  };

  // 切换标签时按新标签重置预填内容。
  const handleLabelChange = (value: string) => {
    const next = value as SampleLabel;
    setLabel(next);
    if (next === 'asr_wrong') {
      setCorrection(segment.text_raw || '');
    } else if (next === 'bad_optimize') {
      setCorrection(segment.text_optimized || '');
    } else {
      // 声纹 / 热词 / ok / other：correction 语义变了（正确说话人 / 正确词 / 无），
      // 残留的整段文本没有意义，清掉。
      setCorrection('');
    }
  };

  // 落库一条样本。面板保存与顶部快捷按钮共用，差别只在传入的标签/纠正内容。
  const submitMark = async (args: {
    label: SampleLabel;
    correction: string | null;
    note: string | null;
    syncHotword: boolean;
  }) => {
    setSaving(true);
    setMarkError('');
    try {
      const segId = (segment.segment_id ?? segment.id) ?? 0;
      const result = await SpeechAPI.markSample({
        segmentId: segId,
        textRaw: segment.text_raw || '',
        textOptimized: segment.text_optimized ?? null,
        textEnglish: segment.text_english ?? null,
        textSecondary: segment.text_secondary ?? null,
        label: args.label,
        correction: args.correction,
        note: args.note,
        syncHotword: args.syncHotword,
        // 说话人快照：任何标签都带，声纹样本靠它记「错成谁」。
        speaker: segment.speaker ?? null,
      });
      setMarkResult(result);
      setMarked(true);
      setPanelOpen(false);
    } catch (err) {
      setMarkError(typeof err === 'string' ? err : (err as Error)?.message || String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleSaveMark = () =>
    submitMark({
      label,
      correction: label === 'ok' || label === 'other' ? null : (correction || null),
      note: label === 'other' ? (note || null) : null,
      syncHotword: label === 'hotword' ? syncHotword : false,
    });

  // 顶部快捷按钮：一键标「声纹识别错误」，不填正确说话人（只记「这段认错了」）。
  // 想指明应该是谁，走标注面板选同一标签再填。
  const handleQuickSpeakerWrong = () => {
    setLabel('speaker_wrong');
    setCorrection('');
    return submitMark({ label: 'speaker_wrong', correction: null, note: null, syncHotword: false });
  };

  const speakerMarked = markResult?.label === 'speaker_wrong';

  const showSecondaryRow = showSecondary && !!segment.text_secondary;

  const optimizeRunning = segment.optimize_status === 'running' || segment.optimize_status === 'pending';
  const translateRunning = segment.translate_status === 'running' || segment.translate_status === 'pending';
  const isProcessing = optimizeRunning || translateRunning;
  const duration = segment.end - segment.start;

  return (
    <>
      <div
        className={cn(
          'group relative flex flex-col p-4 px-4.5 gap-2.5 bg-[var(--bg-card)] border border-[var(--line)] rounded-[16px] shadow-[var(--shadow-sm)] transition-shadow transition-colors animate-fade-up',
          'hover:shadow-[var(--shadow-md)] hover:border-[var(--line-strong)]'
        )}
      >
        <div className="flex items-center gap-3">
          <span className="px-2 py-0.5 rounded-md bg-[var(--bg-soft)] font-mono text-[11px] text-[var(--ink-2)]">
            {stripYear(segment.wall_start)} → {stripYear(segment.wall_end)}
          </span>
          <span className="text-[11px] text-[var(--ink-4)]">{duration.toFixed(1)}s</span>
          {segment.speaker && (
            <span className="px-2 py-0.5 rounded-md bg-[var(--accent-soft,var(--bg-soft))] text-[11px] text-[var(--accent,var(--ink-2))] inline-flex items-center gap-1">
              <Icon name="user" size={11} />
              {segment.speaker}
            </span>
          )}

          <div className="flex-1" />

          <div
            className={cn(
              'flex items-center gap-1 transition-opacity',
              // 播放中 / 加载中保持可见，避免按钮随 hover 消失后停不下来。
              audioState === 'idle' && 'opacity-0 group-hover:opacity-100'
            )}
          >
            <Button
              variant="ghost"
              size="sm"
              className={cn(
                'h-7 px-2 text-[11px] gap-1.5 transition-colors',
                audioState === 'playing' && 'text-[var(--primary-deep)] bg-[var(--primary-soft)]'
              )}
              onClick={handleAudition}
              title="试听这段识别的原始音频（服务端保留 1 天）"
            >
              <Icon
                name={audioState === 'loading' ? 'refresh' : audioState === 'playing' ? 'stop' : 'play'}
                size={12}
                className={cn(audioState === 'loading' && 'animate-spin')}
              />
              {audioState === 'playing' ? '停止' : audioState === 'loading' ? '加载中' : '试听'}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className={cn('h-7 px-2 text-[11px] gap-1.5 transition-colors', copiedZh && 'text-green-600 bg-green-50')}
              disabled={!segment.text_optimized && !segment.text_raw}
              onClick={handleCopyZh}
              title="复制中文"
            >
              <Icon name={copiedZh ? 'check' : 'copy'} size={12} />
              {copiedZh ? '已复制' : '复制中文'}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className={cn('h-7 px-2 text-[11px] gap-1.5 transition-colors', copiedEn && 'text-green-600 bg-green-50')}
              disabled={!segment.text_english}
              onClick={handleCopyEn}
              title="复制英文"
            >
              <Icon name={copiedEn ? 'check' : 'languages'} size={12} />
              {copiedEn ? '已复制' : '复制英文'}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className={cn(
                'h-7 px-2 text-[11px] gap-1.5 transition-colors',
                speakerMarked && 'text-[var(--primary-deep)]'
              )}
              disabled={saving}
              onClick={handleQuickSpeakerWrong}
              title={
                segment.speaker
                  ? `一键标注：声纹认错（当前识别为 ${segment.speaker}）`
                  : '一键标注：声纹识别错误'
              }
            >
              <Icon name={speakerMarked ? 'check' : 'user'} size={12} />
              {speakerMarked ? '已标声纹错' : '声纹错'}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className={cn('h-7 px-2 text-[11px] gap-1.5 transition-colors', marked && 'text-[var(--primary-deep)]')}
              onClick={() => (panelOpen ? setPanelOpen(false) : openPanel())}
              title="标注样本"
            >
              <Icon name={marked ? 'check' : 'tag'} size={12} />
              {marked ? '已标注' : '标注'}
            </Button>
          </div>
        </div>

        <div className="flex flex-col gap-1.5">
          <p className="text-[13px] leading-[1.7] text-[var(--ink-2)] break-words text-pretty">{segment.text_raw}</p>

          {showSecondaryRow && (
            <div className="flex items-start gap-2 px-2.5 py-1.5 rounded-md bg-[var(--bg-soft)] border-l-2 border-[var(--accent,var(--primary))]">
              <span
                className="shrink-0 mt-0.5 text-[10px] uppercase tracking-wider font-medium text-[var(--ink-4)] font-mono"
                title={segment.secondary_kind ? `次模型: ${segment.secondary_kind}` : '次模型'}
              >
                {segment.secondary_kind || '次模型'}
              </span>
              <p className="flex-1 text-[12.5px] leading-[1.6] text-[var(--ink-3)] break-words text-pretty">
                {segment.text_secondary}
              </p>
              <button
                onClick={handleCopySec}
                className="shrink-0 w-6 h-6 rounded flex items-center justify-center text-[var(--ink-4)] hover:text-[var(--primary-deep)] hover:bg-[var(--bg-card)] opacity-0 group-hover:opacity-100 transition-opacity"
                title="复制次模型识别"
              >
                <Icon name={copiedSec ? 'check' : 'copy'} size={11} />
              </button>
            </div>
          )}

          <p className={cn('text-[15px] leading-[1.7] break-words text-pretty', optimizeRunning && 'text-[var(--ink-4)]')}>
            {segment.optimize_status === 'failed'
              ? '优化失败'
              : segment.text_optimized || (optimizeRunning ? '优化中...' : segment.text_raw)}
          </p>

          {showEnglish && (
            <p className={cn('text-[14px] leading-[1.7] break-words text-pretty', translateRunning && 'text-[var(--ink-4)]')}>
              {segment.translate_status === 'failed'
                ? '翻译失败，已保留优化文本'
                : segment.text_english || (translateRunning ? '翻译中...' : '')}
            </p>
          )}
        </div>

        {/* 快捷按钮失败时面板没开，错误也要看得见。 */}
        {markError && !panelOpen && (
          <p className="text-[11px] text-[var(--danger)]">标注失败: {markError}</p>
        )}

        {/* 试听失败提示（音频过期 / 网络问题）。 */}
        {audioError && (
          <p className="text-[11px] text-[var(--danger)]">试听失败: {audioError}</p>
        )}

        {/* 标注结果小字（保存成功后展示）。 */}
        {marked && markResult && !panelOpen && (
          <div className="flex flex-wrap items-center gap-2 text-[11px] text-[var(--ink-4)]">
            <span className="px-1.5 py-0.5 rounded bg-[var(--primary-soft)] text-[var(--primary-deep)]">
              已标注 · {LABEL_OPTIONS.find((o) => o.value === markResult.label)?.label || markResult.label}
            </span>
            {markResult.label === 'speaker_wrong' && markResult.speaker && (
              <span>
                识别为 {markResult.speaker}
                {markResult.correction ? ` → 应为 ${markResult.correction}` : ''}
              </span>
            )}
            <span>{AUDIO_STATUS_TEXT[markResult.audio_status] || markResult.audio_status}</span>
            {markResult.hotword_sync && (
              <span>{HOTWORD_SYNC_TEXT[markResult.hotword_sync] || markResult.hotword_sync}</span>
            )}
          </div>
        )}

        {/* 行内标注面板（轻量，非模态）。 */}
        {panelOpen && (
          <div className="flex flex-col gap-2.5 mt-1 p-3 rounded-[12px] bg-[var(--bg-soft)] border border-[var(--line)]">
            <Dropdown
              label="标注类型"
              options={LABEL_OPTIONS}
              value={label}
              onChange={handleLabelChange}
              className="max-w-[220px]"
            />

            {label === 'asr_wrong' && (
              <textarea
                value={correction}
                onChange={(e) => setCorrection(e.target.value)}
                placeholder="音频真实文本（整段 ground-truth）"
                rows={2}
                className="w-full px-3 py-2 text-[13px] rounded-lg bg-[var(--bg-card)] border border-[var(--line)] text-[var(--ink-2)] resize-y focus:outline-none focus:border-[var(--primary)]"
              />
            )}

            {label === 'speaker_wrong' && (
              <div className="flex flex-col gap-1.5">
                <input
                  value={correction}
                  onChange={(e) => setCorrection(e.target.value)}
                  placeholder="正确的说话人（不确定可留空，只记「这段认错了」）"
                  className="w-full h-9 px-3 text-[13px] rounded-lg bg-[var(--bg-card)] border border-[var(--line)] text-[var(--ink-2)] focus:outline-none focus:border-[var(--primary)]"
                />
                <p className="text-[11px] text-[var(--ink-4)]">
                  当前识别为 {segment.speaker || '（未识别到说话人）'}；文字识别错请另选「识别错误」。
                </p>
              </div>
            )}

            {label === 'bad_optimize' && (
              <textarea
                value={correction}
                onChange={(e) => setCorrection(e.target.value)}
                placeholder="期望的优化文本"
                rows={2}
                className="w-full px-3 py-2 text-[13px] rounded-lg bg-[var(--bg-card)] border border-[var(--line)] text-[var(--ink-2)] resize-y focus:outline-none focus:border-[var(--primary)]"
              />
            )}

            {label === 'hotword' && (
              <div className="flex flex-col gap-2">
                <input
                  value={correction}
                  onChange={(e) => setCorrection(e.target.value)}
                  placeholder="正确术语，或「错词 → 正确词」"
                  className="w-full h-9 px-3 text-[13px] rounded-lg bg-[var(--bg-card)] border border-[var(--line)] text-[var(--ink-2)] focus:outline-none focus:border-[var(--primary)]"
                />
                <label className="flex items-center gap-2 text-[12px] text-[var(--ink-3)] cursor-pointer">
                  <Switch checked={syncHotword} onCheckedChange={setSyncHotword} />
                  同步进热词表
                </label>
              </div>
            )}

            {label === 'other' && (
              <textarea
                value={note}
                onChange={(e) => setNote(e.target.value)}
                placeholder="备注（自由文本）"
                rows={2}
                className="w-full px-3 py-2 text-[13px] rounded-lg bg-[var(--bg-card)] border border-[var(--line)] text-[var(--ink-2)] resize-y focus:outline-none focus:border-[var(--primary)]"
              />
            )}

            {markError && (
              <p className="text-[11px] text-[var(--danger)]">标注失败: {markError}</p>
            )}

            <div className="flex items-center gap-2">
              <Button variant="primary" size="sm" disabled={saving} onClick={handleSaveMark}>
                {saving ? '保存中...' : '保存标注'}
              </Button>
              <Button variant="ghost" size="sm" disabled={saving} onClick={() => setPanelOpen(false)}>
                取消
              </Button>
            </div>
          </div>
        )}

        {isProcessing && (
          <div className="absolute top-4 right-4">
            <Icon name="refresh" size={14} className="animate-spin text-[var(--warning)]" />
          </div>
        )}
      </div>
    </>
  );
};

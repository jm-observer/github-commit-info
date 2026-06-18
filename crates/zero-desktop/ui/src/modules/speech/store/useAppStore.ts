import { useState, useEffect, useCallback, useRef } from 'react';
import { SpeechAPI, type SegmentDiscardedEvent, type SegmentUpdatedEvent } from '../api/tauri-client';
import type { Segment } from '../api/tauri-client';
import { listen } from '@tauri-apps/api/event';

export type AppStatus = 'idle' | 'initializing' | 'recording' | 'processing' | 'error' | 'finished';

/// 客户端兜底超时：某段的优化/翻译停在 running/pending 超过此时长仍无结果，
/// 就本地判为 failed，避免 orchestrator 卡死时 UI 永久转圈（治本在上游 streaming-speech，
/// 见其 docs/todo-2026-06-18-optimize-hang-no-trace.md）。取宽松值，不误伤正常 LLM 延迟。
const STAGE_TIMEOUT_MS = 30_000;
/// 兜底超时扫描间隔。
const STAGE_SWEEP_MS = 2_000;

/// Stable ordering for the segment list. Backend assigns a monotonic
/// `revision` (orchestrator's segment id), so prefer that — it keeps
/// preloaded history and live segments in chronological order even if
/// their wall-clock strings disagree on format. Fall back to wall_start
/// then start_sec for the (rare) cases without a revision.
function compareSegments(a: Segment, b: Segment): number {
  const ar = a.revision ?? a.segment_id ?? a.id;
  const br = b.revision ?? b.segment_id ?? b.id;
  if (typeof ar === 'number' && typeof br === 'number' && ar !== br) {
    return ar - br;
  }
  if (a.wall_start !== b.wall_start) {
    return a.wall_start.localeCompare(b.wall_start);
  }
  return a.start - b.start;
}

export interface UseAppStoreOptions {
  /// 优化稿（`optimize_status==="success"`）首次就绪时回调，每个 revision 仅触发一次。
  /// 用 ref 持有避免重订阅；向后兼容（不传则无副作用）。供语音指令通道做唤醒词门控。
  onOptimizedText?: (text: string, revision: number) => void;
}

export const useAppStore = (options: UseAppStoreOptions = {}) => {
  const [status, setStatus] = useState<AppStatus>('initializing');
  const [errorMessage, setErrorMessage] = useState<string>('');
  const [segments, setSegments] = useState<Segment[]>([]);
  const [devices, setDevices] = useState<{ label: string; value: string }[]>([]);
  const [selectedDevice, setSelectedDevice] = useState<string>('');
  const [showEnglish, setShowEnglish] = useState(true);
  const [isInitialized, setIsInitialized] = useState(false);

  // 优化稿回调用 ref 持有，避免每次 prop 变化都重订阅事件。
  const onOptimizedTextRef = useRef(options.onOptimizedText);
  onOptimizedTextRef.current = options.onOptimizedText;
  // 已就绪触发过的 revision，避免重复触发（优化稿可能多次写入）。
  const optimizedFiredRef = useRef<Set<number>>(new Set());

  // 兜底超时用：每个 revision 首次出现的时刻 + 已判超时的阶段集合。
  // 判超时后钉死 failed，防 orchestrator 因其它子事件重发把该阶段刷回 running。
  const firstSeenAtRef = useRef<Map<number, number>>(new Map());
  const optTimedOutRef = useRef<Set<number>>(new Set());
  const trTimedOutRef = useRef<Set<number>>(new Set());

  const mapDbSegment = useCallback((row: Record<string, unknown>): Segment => ({
    id: typeof row.id === 'number' ? row.id : null,
    segment_id: typeof row.segment_id === 'number' ? row.segment_id : null,
    revision: typeof row.revision === 'number' ? row.revision : undefined,
    start: typeof row.start_sec === 'number' ? row.start_sec : 0,
    end: typeof row.end_sec === 'number' ? row.end_sec : 0,
    wall_start: typeof row.wall_start === 'string' ? row.wall_start : '',
    wall_end: typeof row.wall_end === 'string' ? row.wall_end : '',
    text_raw: typeof row.text_raw === 'string' ? row.text_raw : '',
    text_optimized: typeof row.text_optimized === 'string' ? row.text_optimized : undefined,
    text_english: typeof row.text_english === 'string' ? row.text_english : undefined,
    text_secondary: typeof row.text_secondary === 'string' ? row.text_secondary : undefined,
    secondary_kind: typeof row.secondary_kind === 'string' ? row.secondary_kind : undefined,
    speaker: typeof row.speaker === 'string' && row.speaker.length > 0 ? row.speaker : undefined,
    optimize_status:
      row.optimize_status === 'pending' ||
      row.optimize_status === 'running' ||
      row.optimize_status === 'success' ||
      row.optimize_status === 'failed'
        ? row.optimize_status
        : 'pending',
    translate_status:
      row.translate_status === 'blocked' ||
      row.translate_status === 'pending' ||
      row.translate_status === 'running' ||
      row.translate_status === 'success' ||
      row.translate_status === 'failed'
        ? row.translate_status
        : 'blocked',
  }), []);

  // Map an orchestrator /api/history row (server SegmentRow shape) into the
  // Segment shape the desktop UI consumes. Server fields:
  //   id, session_id, ts, text, optimized, english, speaker, has_audio
  // We use `id` as both segment_id and a synthetic revision so it merges
  // with live `segment_updated` events without colliding.
  const mapServerHistory = useCallback((row: Record<string, unknown>): Segment => {
    const id = typeof row.id === 'number' ? row.id : null;
    const ts = typeof row.ts === 'string' ? row.ts : '';
    return {
      id,
      segment_id: id,
      revision: id ?? undefined,
      start: 0,
      end: 0,
      wall_start: ts,
      wall_end: ts,
      text_raw: typeof row.text === 'string' ? row.text : '',
      text_optimized: typeof row.optimized === 'string' ? row.optimized : undefined,
      text_english: typeof row.english === 'string' ? row.english : undefined,
      text_secondary: typeof row.secondary === 'string' ? row.secondary : undefined,
      speaker: typeof row.speaker === 'string' && row.speaker.length > 0 ? row.speaker : undefined,
      // Server-side history is post-processing — mark both stages as
      // 'success' regardless of whether optimized/english are present, so
      // preloaded rows never show a spinner. SegmentCard falls back to
      // `text_raw` when the optimized field is empty, which is the right
      // affordance for "this is past context, not currently being processed".
      optimize_status: 'success',
      translate_status: 'success',
    };
  }, []);

  // Initialize
  useEffect(() => {
    let canceled = false;
    let initTimer: number | null = null;
    let unsubscribeSegmentDiscarded: (() => void) | null = null;
    let unsubscribeSegmentUpdated: (() => void) | null = null;

    const init = async () => {
      try {
        const deviceList = await SpeechAPI.listDevices();
        if (canceled) return;
        setDevices(deviceList.map(d => ({ label: d.is_default ? `${d.name} (Default)` : d.name, value: d.name })));

        const selected = await SpeechAPI.getSelectedDevice();
        if (canceled) return;
        if (selected) setSelectedDevice(selected);

        // Preload last 5 history segments from the orchestrator so the panel
        // isn't empty before the first recording of this session.
        try {
          const rows = await SpeechAPI.fetchRemoteHistory(5);
          if (canceled) return;
          if (rows.length > 0) {
            const mapped = rows
              .map(mapServerHistory)
              .filter((seg) => seg.text_raw.trim().length > 0)
              .reverse();
            setSegments(mapped);
          }
        } catch (err) {
          console.warn('Preload remote history failed', err);
        }

        pollInit();
      } catch (err) {
        if (canceled) return;
        console.error("Init failed", err);
        setStatus('error');
      }
    };

    const pollInit = async () => {
      if (canceled) return;
      const res = await SpeechAPI.getInitStatus();
      if (canceled) return;
      if (res.status === 1) {
        setStatus((prev) => (prev === 'initializing' ? 'idle' : prev));
      } else if (res.status === 2) {
        setErrorMessage(res.error || '初始化失败');
        setStatus('error');
      } else {
        initTimer = window.setTimeout(pollInit, 500);
      }
    };

    const runInit = async () => {
      await init();
      setIsInitialized(true);
    };
    runInit();

    // Subscribe to segment_discarded events
    void listen<SegmentDiscardedEvent>('segment_discarded', (event) => {
      if (canceled) return;
      const { revision, segment_id } = event.payload;
      console.debug('[segment_discarded]', { revision, segment_id, reason: event.payload.reason });

      setSegments((prev) => {
        const filtered = prev.filter(s => {
          if (segment_id !== null && s.segment_id === segment_id) return false;
          if (s.revision !== undefined && revision !== undefined && s.revision === revision) return false;
          return true;
        });

        return filtered.sort(compareSegments);
      });
    })
      .then((unlisten) => {
        if (canceled) { unlisten(); return; }
        unsubscribeSegmentDiscarded = unlisten;
      })
      .catch((err) => { console.error('Subscribe segment_discarded failed', err); });

    void listen<SegmentUpdatedEvent>('segment_updated', (event) => {
      if (canceled) return;
      const row = event.payload;
      const next = mapDbSegment(row as unknown as Record<string, unknown>);
      if (next.revision === undefined) return;
      console.debug('[segment_updated]', { revision: next.revision, segmentId: next.segment_id });

      const rev = next.revision;
      if (!firstSeenAtRef.current.has(rev)) firstSeenAtRef.current.set(rev, Date.now());
      // 成功 → 清超时标记（迟到结果可恢复）；已判超时 → 钉死 failed，
      // 防 orchestrator 因其它子事件（translated/secondary）重发把该阶段刷回 running。
      if (next.optimize_status === 'success') optTimedOutRef.current.delete(rev);
      else if (optTimedOutRef.current.has(rev)) next.optimize_status = 'failed';
      if (next.translate_status === 'success') trTimedOutRef.current.delete(rev);
      else if (trTimedOutRef.current.has(rev)) next.translate_status = 'failed';

      // 优化稿就绪 → 触发回调（每 revision 一次，供语音门控）。
      if (next.optimize_status === 'success') {
        const text = (next.text_optimized ?? '').trim();
        if (text && !optimizedFiredRef.current.has(next.revision)) {
          optimizedFiredRef.current.add(next.revision);
          onOptimizedTextRef.current?.(text, next.revision);
        }
      }

      setSegments((prev) => {
        const exists = prev.some((segment) => segment.revision === next.revision);
        const updated = exists
          ? prev.map((segment) =>
              segment.revision === next.revision ? { ...segment, ...next } : segment,
            )
          : [...prev, next];

        return updated.sort(compareSegments);
      });
    })
      .then((unlisten) => {
        if (canceled) { unlisten(); return; }
        unsubscribeSegmentUpdated = unlisten;
      })
      .catch((err) => { console.error('Subscribe segment_updated failed', err); });

    return () => {
      canceled = true;
      if (initTimer !== null) window.clearTimeout(initTimer);
      if (typeof unsubscribeSegmentDiscarded === 'function') unsubscribeSegmentDiscarded();
      if (typeof unsubscribeSegmentUpdated === 'function') unsubscribeSegmentUpdated();
    };
  }, [mapDbSegment, mapServerHistory]);

  // 兜底超时扫描：把停在 running/pending 超时仍无结果的阶段判为 failed，
  // 让卡死的段不再无限转圈（翻译卡死同样会停掉卡片右上的 spinner）。
  useEffect(() => {
    const timer = window.setInterval(() => {
      const now = Date.now();
      setSegments((prev) => {
        let mutated = false;
        const out = prev.map((seg) => {
          const rev = seg.revision;
          if (rev === undefined) return seg;
          const since = firstSeenAtRef.current.get(rev);
          if (since === undefined || now - since < STAGE_TIMEOUT_MS) return seg;
          let s = seg;
          if (seg.optimize_status === 'running' || seg.optimize_status === 'pending') {
            optTimedOutRef.current.add(rev);
            s = { ...s, optimize_status: 'failed' };
            mutated = true;
          }
          if (seg.translate_status === 'running' || seg.translate_status === 'pending') {
            trTimedOutRef.current.add(rev);
            s = { ...s, translate_status: 'failed' };
            mutated = true;
          }
          return s;
        });
        return mutated ? out : prev;
      });
    }, STAGE_SWEEP_MS);
    return () => window.clearInterval(timer);
  }, []);

  return {
    status, setStatus,
    errorMessage, setErrorMessage,
    segments, setSegments,
    devices, setDevices,
    selectedDevice, setSelectedDevice,
    showEnglish, setShowEnglish,
    isInitialized,
  };
};

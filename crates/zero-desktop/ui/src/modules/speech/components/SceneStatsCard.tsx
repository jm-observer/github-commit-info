import React, { useCallback, useEffect, useRef, useState } from 'react';
import { SpeechAPI, type SceneStats } from '../api/tauri-client';
import { Icon } from './ui/Icon';

/** 轮询间隔：交付是人说话的节奏（几秒到几十秒一条），10s 足够有「在涨」的实感又不折腾数据库。 */
const POLL_MS = 10_000;
/** 「刚刚 +N」提示的停留时长。 */
const BUMP_HOLD_MS = 6_000;

/**
 * 场景记录面板 —— 让「日常全量收集」这件事看得见。
 *
 * 这块统计的是**每次交付都记**的 speech_scenes，不是手动按 Ctrl+Alt+X 采的纠错样本：
 * 前者回答「我在哪个软件里说什么样的话」，是后续按应用定制中文优化的依据；后者只覆盖出错
 * 的那几条。两者刻意分开展示，免得混为一谈。
 */
export const SceneStatsCard: React.FC = () => {
  const [stats, setStats] = useState<SceneStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [bump, setBump] = useState(0);
  const [titlesOpen, setTitlesOpen] = useState(false);
  const prevTotalRef = useRef<number | null>(null);
  const bumpTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refresh = useCallback(async () => {
    try {
      const next = await SpeechAPI.sceneStats();
      const prev = prevTotalRef.current;
      // 首次加载不算「新增」——那是存量，不是这次涨的。
      if (prev !== null && next.total > prev) {
        setBump(next.total - prev);
        if (bumpTimerRef.current) clearTimeout(bumpTimerRef.current);
        bumpTimerRef.current = setTimeout(() => setBump(0), BUMP_HOLD_MS);
      }
      prevTotalRef.current = next.total;
      setStats(next);
      setError(null);
    } catch (e: any) {
      setError(e?.message || String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => {
      clearInterval(timer);
      if (bumpTimerRef.current) clearTimeout(bumpTimerRef.current);
    };
  }, [refresh]);

  const maxCount = stats?.top_apps?.[0]?.count ?? 0;

  return (
    <div className="rounded-lg border border-[var(--line)] bg-[var(--bg-soft)] p-3">
      <div className="flex items-center justify-between">
        <span className="text-[13px] font-medium text-[var(--ink-2)]">场景记录</span>
        <button
          type="button"
          onClick={() => void refresh()}
          className="rounded p-1 text-[var(--ink-4)] transition-colors hover:text-[var(--ink-2)]"
          title="立即刷新"
        >
          <Icon name="refresh" size={14} />
        </button>
      </div>

      {error && <p className="mt-2 text-[11px] text-red-500">读取失败：{error}</p>}

      {!error && !stats && <p className="mt-2 text-[11px] text-[var(--ink-4)]">加载中…</p>}

      {stats && (
        <>
          <div className="mt-2 flex items-baseline gap-2">
            <span className="text-2xl font-semibold tabular-nums text-[var(--ink)]">
              {stats.total}
            </span>
            <span className="text-[11px] text-[var(--ink-4)]">条交付</span>
            {bump > 0 && (
              <span className="rounded bg-emerald-500/15 px-1.5 py-0.5 text-[11px] font-medium text-emerald-600 dark:text-emerald-400">
                刚刚 +{bump}
              </span>
            )}
          </div>

          <div className="mt-1 text-[11px] leading-relaxed text-[var(--ink-4)]">
            今天 {stats.today} 条 · {stats.distinct_apps} 个应用 ·{' '}
            {stats.total_chars.toLocaleString()} 字
            {stats.last_at && <> · 最近 {stats.last_at.slice(11)}</>}
          </div>

          {/* 被纠错的记录：交付文本含 ASR/LLM 错误，统计真实表达风格时应排除。 */}
          {stats.corrected > 0 && (
            <div className="mt-1 text-[11px] text-[var(--ink-4)]">
              其中 {stats.corrected} 条被纠错（分析表达风格时排除）
            </div>
          )}

          {/* 抓拍覆盖率：抓不到应用的那部分无法参与场景分析，缺口大就该查抓拍点了。 */}
          {stats.total > 0 && stats.with_app < stats.total && (
            <div className="mt-1 text-[11px] text-amber-600 dark:text-amber-400">
              其中 {stats.total - stats.with_app} 条没抓到应用上下文
            </div>
          )}

          {stats.top_apps.length > 0 && (
            <div className="mt-3 flex flex-col gap-1.5">
              {stats.top_apps.map((a) => (
                <div key={a.app_exe ?? '(unknown)'} className="flex items-center gap-2">
                  <span
                    className="w-[104px] shrink-0 truncate text-[11px] text-[var(--ink-3)]"
                    title={a.app_exe ?? '未知应用'}
                  >
                    {a.app_exe ?? '未知'}
                  </span>
                  <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-[var(--line)]">
                    <div
                      className="h-full rounded-full bg-[var(--primary)]"
                      style={{ width: `${maxCount > 0 ? (a.count / maxCount) * 100 : 0}%` }}
                    />
                  </div>
                  <span className="w-8 shrink-0 text-right text-[11px] tabular-nums text-[var(--ink-4)]">
                    {a.count}
                  </span>
                </div>
              ))}
            </div>
          )}

          {/* 按「应用 + 窗口标题」的具体场景。title 基数大、可能含隐私（联系人/文档/网页名），
              默认折叠。展开后是原始聚合，不做域名/文档名解析——归纳粒度等数据摊开再定。 */}
          {stats.top_titles.length > 0 && (
            <div className="mt-3 border-t border-[var(--line)] pt-2">
              <button
                type="button"
                onClick={() => setTitlesOpen((v) => !v)}
                className="flex items-center gap-1 text-[11px] text-[var(--ink-3)] transition-colors hover:text-[var(--ink-2)]"
              >
                <span className="inline-block w-3">{titlesOpen ? '▾' : '▸'}</span>
                按窗口场景（{stats.top_titles.length}）
              </button>
              {titlesOpen && (
                <div className="mt-2 flex flex-col gap-1">
                  {stats.top_titles.map((t) => {
                    const label = `${t.app_exe ?? '未知'} · ${t.app_title ?? '（无标题）'}`;
                    return (
                      <div
                        key={`${t.app_exe ?? ''}|${t.app_title ?? ''}`}
                        className="flex items-baseline justify-between gap-2 text-[11px]"
                      >
                        <span className="truncate text-[var(--ink-3)]" title={label}>
                          {label}
                        </span>
                        <span className="shrink-0 tabular-nums text-[var(--ink-4)]">{t.count}</span>
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          )}

          {stats.total === 0 && (
            <p className="mt-2 text-[11px] leading-relaxed text-[var(--ink-4)]">
              还没有记录。开启自动粘贴或自动复制后，每次语音结果落进其他应用都会记一条。
            </p>
          )}
        </>
      )}
    </div>
  );
};

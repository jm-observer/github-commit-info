import React, { useState } from 'react';
import { Icon } from './ui/Icon';
import { cn } from '../utils';

interface RemoteUrlPickerProps {
  /** Currently selected URL (one of `presets` or the built-in default). */
  value: string;
  /** User-added presets — built-in default is shown separately and is never in this list. */
  presets: string[];
  /** 「内置」项地址：由 G10 局域网/外网设置派生，不再是编译期常量。 */
  builtinUrl: string;
  /** Disable interactions (e.g. while busy applying). Selecting a different URL
   * is still allowed during recording — App handles the stop+restart. */
  disabled?: boolean;
  /** Pick an existing URL. */
  onSelect: (url: string) => void;
  /** Append a new custom URL to the preset list AND select it. */
  onAdd: (url: string) => void;
  /** Remove a user-added preset. If it was selected, caller should fall back
   *  to the built-in default. */
  onRemove: (url: string) => void;
}

/** Tiny URL sanity check matching the backend's `validate_url`. */
function isValidWsUrl(s: string): boolean {
  const t = s.trim();
  return t.startsWith('ws://') || t.startsWith('wss://');
}

/** Dropdown for the orchestrator connection URL.
 *
 *  Surfaces the built-in default + user presets, with inline add/delete.
 *  A delete icon appears only on user-added items (the built-in default
 *  cannot be removed). Adding switches the selection to the new URL.
 */
export const RemoteUrlPicker: React.FC<RemoteUrlPickerProps> = ({
  value,
  presets,
  builtinUrl,
  disabled,
  onSelect,
  onAdd,
  onRemove,
}) => {
  const [isOpen, setIsOpen] = useState(false);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState('');
  const [draftError, setDraftError] = useState('');

  // Built-in always shown first; then user presets (de-duped + minus default).
  const items: { url: string; builtin: boolean }[] = [
    { url: builtinUrl, builtin: true },
    ...presets
      .filter((p) => p && p !== builtinUrl)
      .map((p) => ({ url: p, builtin: false })),
  ];

  const closeAll = () => {
    setIsOpen(false);
    setAdding(false);
    setDraft('');
    setDraftError('');
  };

  const submitDraft = () => {
    const t = draft.trim();
    if (!isValidWsUrl(t)) {
      setDraftError('需以 ws:// 或 wss:// 开头');
      return;
    }
    onAdd(t);
    closeAll();
  };

  return (
    <div className="relative w-full">
      <label className="block text-[11.5px] font-medium uppercase tracking-wider text-[var(--ink-3)] mb-1.5 ml-1">
        连接地址
      </label>
      <button
        disabled={disabled}
        onClick={() => setIsOpen((v) => !v)}
        className={cn(
          'flex w-full items-center justify-between h-9 px-3.5 bg-[var(--bg-card)] border border-[var(--line)] rounded-lg text-[13px] text-[var(--ink-2)] transition-all',
          'hover:border-[var(--line-strong)] hover:bg-[var(--bg-soft)]',
          'disabled:opacity-50 disabled:cursor-not-allowed',
          isOpen && 'ring-2 ring-[var(--primary-soft)] border-[var(--primary)]'
        )}
        title={value}
      >
        <div className="flex items-center gap-2 truncate">
          <Icon name="device" size={15} className="text-[var(--ink-3)]" />
          <span className="truncate font-mono text-[12px]">{value || '未配置'}</span>
        </div>
        <Icon
          name="chevron-down"
          size={14}
          className={cn('text-[var(--ink-4)] transition-transform', isOpen && 'rotate-180')}
        />
      </button>

      {isOpen && !disabled && (
        <>
          <div className="fixed inset-0 z-10" onClick={closeAll} />
          <div className="absolute top-full left-0 right-0 mt-1.5 z-20 bg-[var(--bg-card)] border border-[var(--line)] rounded-xl shadow-[var(--shadow-md)] overflow-hidden animate-fade-up">
            <div className="max-h-[240px] overflow-y-auto">
              {items.map((it) => {
                const selected = it.url === value;
                return (
                  <div
                    key={it.url}
                    className={cn(
                      'group flex items-center gap-1 px-2 py-1 transition-colors',
                      selected
                        ? 'bg-[var(--primary-soft)]'
                        : 'hover:bg-[var(--bg-soft)]'
                    )}
                  >
                    <button
                      onClick={() => {
                        onSelect(it.url);
                        closeAll();
                      }}
                      className={cn(
                        'flex-1 min-w-0 text-left px-2 py-1.5 text-[12px] font-mono truncate',
                        selected
                          ? 'text-[var(--primary-deep)] font-medium'
                          : 'text-[var(--ink-2)]'
                      )}
                      title={it.url}
                    >
                      {it.url}
                      {it.builtin && (
                        <span className="ml-2 font-sans text-[10px] uppercase tracking-wider text-[var(--ink-4)]">
                          内置
                        </span>
                      )}
                    </button>
                    {!it.builtin && (
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          onRemove(it.url);
                          if (selected) onSelect(builtinUrl);
                        }}
                        className="shrink-0 w-7 h-7 rounded-md flex items-center justify-center text-[var(--ink-4)] hover:text-[var(--danger)] hover:bg-[var(--danger-soft)] opacity-0 group-hover:opacity-100 transition-opacity"
                        title="删除此预设"
                      >
                        <Icon name="trash" size={13} />
                      </button>
                    )}
                  </div>
                );
              })}
            </div>

            <div className="border-t border-[var(--line)]">
              {!adding ? (
                <button
                  onClick={() => setAdding(true)}
                  className="w-full text-left px-4 py-2.5 text-[12.5px] text-[var(--ink-3)] hover:bg-[var(--bg-soft)] hover:text-[var(--primary-deep)] flex items-center gap-2"
                >
                  <Icon name="plus" size={13} />
                  添加自定义地址…
                </button>
              ) : (
                <div className="px-3 py-2.5 flex flex-col gap-2">
                  <input
                    autoFocus
                    type="text"
                    placeholder="ws://host:port/stream"
                    value={draft}
                    onChange={(e) => {
                      setDraft(e.target.value);
                      if (draftError) setDraftError('');
                    }}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') {
                        e.preventDefault();
                        submitDraft();
                      } else if (e.key === 'Escape') {
                        closeAll();
                      }
                    }}
                    className={cn(
                      'h-8 px-2.5 bg-[var(--bg-card)] border rounded-md text-[12px] font-mono text-[var(--ink-2)] focus:outline-none focus:ring-2',
                      draftError
                        ? 'border-[var(--danger)] focus:ring-[var(--danger-soft)]'
                        : 'border-[var(--line)] focus:ring-[var(--primary-soft)] focus:border-[var(--primary)]'
                    )}
                  />
                  {draftError && (
                    <span className="text-[11px] text-[var(--danger)] ml-0.5">{draftError}</span>
                  )}
                  <div className="flex justify-end gap-1.5">
                    <button
                      onClick={closeAll}
                      className="h-7 px-3 rounded-md text-[12px] text-[var(--ink-3)] hover:bg-[var(--bg-soft)]"
                    >
                      取消
                    </button>
                    <button
                      onClick={submitDraft}
                      className="h-7 px-3 rounded-md text-[12px] font-medium bg-[var(--primary)] text-white hover:bg-[var(--primary-deep)]"
                    >
                      保存
                    </button>
                  </div>
                </div>
              )}
            </div>
          </div>
        </>
      )}
    </div>
  );
};

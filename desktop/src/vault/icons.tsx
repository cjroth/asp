// Inline SVG icons ported from the design. Each takes an optional size/stroke.
import React from 'react';

type P = { size?: number; stroke?: string; style?: React.CSSProperties };

const svg = (size: number, vb: string, extra: Partial<React.SVGProps<SVGSVGElement>>, children: React.ReactNode, style?: React.CSSProperties) => (
  <svg width={size} height={size} viewBox={vb} fill="none" style={style} {...extra}>
    {children}
  </svg>
);

export const FileIcon = ({ size = 13, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 14 14', { stroke, strokeWidth: 1.25, strokeLinejoin: 'round' }, (
    <>
      <path d="M3.5 1.75 H8.5 L11 4.25 V12.25 H3.5 Z" />
      <path d="M8.25 1.9 V4.4 H10.75" />
    </>
  ), style);

export const ChevronRight = ({ size = 11, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 12 12', { stroke, strokeWidth: 1.6, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M4 2.5 L8 6 L4 9.5" />, style);

export const CaretDown = ({ size = 13, stroke = '#b8b3ac', style }: P) =>
  svg(size, '0 0 14 14', { stroke, strokeWidth: 1.5, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M3.5 5.5 L7 9 L10.5 5.5" />, style);

export const PlusIcon = ({ size = 16, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.5, strokeLinecap: 'round' }, <path d="M8 4 V12 M4 8 H12" />, style);

export const FolderIcon = ({ size = 15, stroke = '#a8a29e', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.4, strokeLinejoin: 'round' }, <path d="M2 4.5 a1 1 0 0 1 1 -1 H6 L7.5 5 H13 a1 1 0 0 1 1 1 V11.5 a1 1 0 0 1 -1 1 H3 a1 1 0 0 1 -1 -1 Z" />, style);

export const ShareIcon = ({ size = 15, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.5, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
    <>
      <circle cx="4" cy="8" r="1.6" />
      <circle cx="12" cy="3.6" r="1.6" />
      <circle cx="12" cy="12.4" r="1.6" />
      <path d="M5.4 7.2 L10.6 4.4 M5.4 8.8 L10.6 11.6" />
    </>
  ), style);

export const TrashIcon = ({ size = 15, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.4, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M3 4.5 H13 M6.5 4.5 V3 H9.5 V4.5 M4.5 4.5 L5 13 H11 L11.5 4.5" />, style);

export const PencilIcon = ({ size = 14, stroke = '#78716c', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.4, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M9.5 3 L13 6.5 L6 13.5 H2.5 V10 Z" />, style);

export const CheckIcon = ({ size = 15, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.8, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M3.5 8.5 L6.5 11.5 L12.5 4.5" />, style);

export const ClockIcon = ({ size = 14, stroke = '#a8a29e', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.4, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 4.5 V8 L10.3 9.6" />
    </>
  ), style);

export const MinusIcon = ({ size = 14, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.6, strokeLinecap: 'round' }, <path d="M4 8 H12" />, style);

export const UserIcon = ({ size = 12, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 14 14', { stroke, strokeWidth: 1.3 }, (
    <>
      <circle cx="7" cy="4.6" r="2.5" />
      <path d="M2.6 12.2 a4.4 4.4 0 0 1 8.8 0" />
    </>
  ), style);

export const LinkIcon = ({ size = 16, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.5, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M6 10 L10 6 M6.5 3.5 L8 2 a2.8 2.8 0 0 1 4 4 L10.5 7.5 M9.5 12.5 L8 14 a2.8 2.8 0 0 1 -4 -4 L5.5 8.5" />, style);

export const DesktopIcon = ({ size = 13, stroke = '#a8a29e', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.3, strokeLinejoin: 'round' }, (
    <>
      <rect x="2" y="3" width="12" height="8" rx="1" />
      <path d="M6 13.5 H10 M8 11 V13.5" />
    </>
  ), style);

// ---- icons added for the new design ----

// Theme toggle: a moon (when in dark mode) or a sun (when in light mode).
export const ThemeIcon = ({ size = 16, dark = false, style }: P & { dark?: boolean }) =>
  dark
    ? svg(size, '0 0 16 16', { stroke: 'currentColor', strokeWidth: 1.4, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M13.5 9.2 A5.2 5.2 0 1 1 6.8 2.5 A4 4 0 0 0 13.5 9.2 Z" />, style)
    : svg(size, '0 0 16 16', { stroke: 'currentColor', strokeWidth: 1.4, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
        <>
          <circle cx="8" cy="8" r="3.1" />
          <path d="M8 1.5V3 M8 13V14.5 M1.5 8H3 M13 8H14.5 M3.4 3.4 L4.5 4.5 M11.5 11.5 L12.6 12.6 M12.6 3.4 L11.5 4.5 M4.5 11.5 L3.4 12.6" />
        </>
      ), style);

export const DotsIcon = ({ size = 16, style }: P) => (
  <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" style={style}>
    <circle cx="8" cy="3.2" r="1.35" />
    <circle cx="8" cy="8" r="1.35" />
    <circle cx="8" cy="12.8" r="1.35" />
  </svg>
);

export const NewFileIcon = ({ size = 15, stroke = '#78716c', style }: P) =>
  svg(size, '0 0 24 24', { stroke, strokeWidth: 1.8, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
    <>
      <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" />
      <path d="M14 2v6h6" />
      <path d="M12 12v6" />
      <path d="M9 15h6" />
    </>
  ), style);

export const NewFolderIcon = ({ size = 15, stroke = '#78716c', style }: P) =>
  svg(size, '0 0 24 24', { stroke, strokeWidth: 1.8, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
    <>
      <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
      <path d="M12 10v6" />
      <path d="M9 13h6" />
    </>
  ), style);

export const ExpandCollapseIcon = ({ size = 16, expanded = false, style }: P & { expanded?: boolean }) =>
  svg(size, '0 0 24 24', { stroke: 'currentColor', strokeWidth: 2, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
    expanded ? (
      <>
        <path d="M7 20l5-5 5 5" />
        <path d="M7 4l5 5 5-5" />
      </>
    ) : (
      <>
        <path d="M7 15l5 5 5-5" />
        <path d="M7 9l5-5 5 5" />
      </>
    )
  ), style);

export const EyeIcon = ({ size = 15, off = false, stroke = 'currentColor', style }: P & { off?: boolean }) =>
  off
    ? svg(size, '0 0 16 16', { stroke, strokeWidth: 1.5, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
        <>
          <path d="M1.5 8 C3 5 5.3 3.8 8 3.8 C10.7 3.8 13 5 14.5 8 C13 11 10.7 12.2 8 12.2 C5.3 12.2 3 11 1.5 8 Z" />
          <path d="M3 3 L13 13" />
        </>
      ), style)
    : svg(size, '0 0 16 16', { stroke, strokeWidth: 1.5, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
        <>
          <path d="M1.5 8 C3 5 5.3 3.8 8 3.8 C10.7 3.8 13 5 14.5 8 C13 11 10.7 12.2 8 12.2 C5.3 12.2 3 11 1.5 8 Z" />
          <circle cx="8" cy="8" r="2" />
        </>
      ), style);

export const ListIcon = ({ size = 13, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 24 24', { stroke, strokeWidth: 2, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M4 6h16M4 12h16M4 18h10" />, style);

export const CopyIcon = ({ size = 15, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.4, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
    <>
      <rect x="5.5" y="5.5" width="8" height="8" rx="1.5" />
      <path d="M3.5 10.5 H3 a1.5 1.5 0 0 1 -1.5 -1.5 V3 a1.5 1.5 0 0 1 1.5 -1.5 H9 a1.5 1.5 0 0 1 1.5 1.5 V3.5" />
    </>
  ), style);

export const WandIcon = ({ size = 15, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.5, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M10.5 2.5 L13.5 5.5 M11.8 1.2 a1.6 1.6 0 0 1 2.3 2.3 L4.6 13 L1.5 14 L2.5 10.9 Z" />, style);

export const XIcon = ({ size = 11, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.6, strokeLinecap: 'round' }, <path d="M4 4 L12 12 M12 4 L4 12" />, style);

// "Connect a vault" — a chain-link glyph (the new design's connect button icon).
export const ConnectIcon = ({ size = 15, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.5, strokeLinecap: 'round', strokeLinejoin: 'round' }, <path d="M6 10 L10 6 M6.5 3.5 L8 2 a2.8 2.8 0 0 1 4 4 L10.5 7.5 M9.5 12.5 L8 14 a2.8 2.8 0 0 1 -4 -4 L5.5 8.5" />, style);

// Globe — a web/browser-storage vault (no folder on disk).
export const GlobeIcon = ({ size = 12, stroke = 'currentColor', style }: P) =>
  svg(size, '0 0 16 16', { stroke, strokeWidth: 1.2, strokeLinecap: 'round', strokeLinejoin: 'round' }, (
    <>
      <circle cx="8" cy="8" r="6.4" />
      <path d="M1.7 8 H14.3" />
      <path d="M8 1.6 C5.4 4 5.4 12 8 14.4 M8 1.6 C10.6 4 10.6 12 8 14.4" />
    </>
  ), style);

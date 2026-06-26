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

import type { ReactNode } from "react";

interface IconProps {
  size?: number;
  className?: string;
}

function svg(path: ReactNode, viewBox = "0 0 16 16") {
  return function Icon({ size = 14, className }: IconProps) {
    return (
      <svg
        width={size}
        height={size}
        viewBox={viewBox}
        fill="none"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinecap="round"
        strokeLinejoin="round"
        className={className}
        aria-hidden="true"
      >
        {path}
      </svg>
    );
  };
}

export const IconDatabase = svg(
  <>
    <ellipse cx="8" cy="3.6" rx="5.2" ry="2.1" />
    <path d="M2.8 3.6v8.8c0 1.16 2.33 2.1 5.2 2.1s5.2-.94 5.2-2.1V3.6" />
    <path d="M2.8 8c0 1.16 2.33 2.1 5.2 2.1s5.2-.94 5.2-2.1" />
  </>,
);

export const IconServer = svg(
  <>
    <rect x="2" y="2.5" width="12" height="4.6" rx="1.2" />
    <rect x="2" y="8.9" width="12" height="4.6" rx="1.2" />
    <path d="M4.6 4.8h.01M4.6 11.2h.01" />
  </>,
);

export const IconTable = svg(
  <>
    <rect x="2" y="2.6" width="12" height="10.8" rx="1.2" />
    <path d="M2 6.2h12M6.4 6.2v7.2M10.4 6.2v7.2" />
  </>,
);

export const IconView = svg(
  <>
    <path d="M1.6 8s2.6-4.4 6.4-4.4S14.4 8 14.4 8s-2.6 4.4-6.4 4.4S1.6 8 1.6 8Z" />
    <circle cx="8" cy="8" r="1.9" />
  </>,
);

export const IconFolder = svg(
  <path d="M1.8 4.2c0-.7.55-1.2 1.2-1.2h2.6l1.4 1.6h5.2c.65 0 1.2.5 1.2 1.2v6.2c0 .7-.55 1.2-1.2 1.2H3c-.65 0-1.2-.5-1.2-1.2V4.2Z" />,
);

export const IconChevronRight = svg(<path d="M6 3.6 10.4 8 6 12.4" />);

export const IconArrowUp = svg(
  <>
    <path d="M8 13V3.4" />
    <path d="M3.8 7.6 8 3.4l4.2 4.2" />
  </>,
);

export const IconArrowDown = svg(
  <>
    <path d="M8 3v9.6" />
    <path d="M3.8 8.4 8 12.6l4.2-4.2" />
  </>,
);
export const IconChevronDown = svg(<path d="M3.6 6 8 10.4 12.4 6" />);

export const IconPlus = svg(<path d="M8 3v10M3 8h10" />);
export const IconMinus = svg(<path d="M3 8h10" />);
export const IconClose = svg(<path d="M4 4l8 8M12 4l-8 8" />);
export const IconCheck = svg(<path d="M3 8.6 6.3 12 13 4.6" />);

export const IconRefresh = svg(
  <>
    <path d="M13.4 8a5.4 5.4 0 1 1-1.6-3.85" />
    <path d="M13.6 2.4v3.2h-3.2" />
  </>,
);

export const IconTrash = svg(
  <>
    <path d="M2.8 4.2h10.4" />
    <path d="M6.4 4.2V2.9c0-.4.3-.7.7-.7h1.8c.4 0 .7.3.7.7v1.3" />
    <path d="M4.2 4.2l.6 8.4c0 .6.5 1.1 1.1 1.1h4.2c.6 0 1.1-.5 1.1-1.1l.6-8.4" />
  </>,
);

export const IconEdit = svg(
  <>
    <path d="M11.2 2.6l2.2 2.2-8 8-2.9.7.7-2.9 8-8Z" />
  </>,
);

export const IconPlay = svg(<path d="M4.6 3.2 12.6 8l-8 4.8V3.2Z" />);

export const IconEqual = svg(
  <>
    <path d="M2.6 5.4h10.8M2.6 10.6h10.8" />
  </>,
);

export const IconCompare = svg(
  <>
    <path d="M4.6 2.4v11.2M11.4 2.4v11.2" />
    <path d="M2 5.6h5.2M8.8 10.4H14" />
  </>,
);

export const IconDownload = svg(
  <>
    <path d="M8 2.4v7.6" />
    <path d="M4.8 7.2 8 10.4l3.2-3.2" />
    <path d="M2.6 12.8h10.8" />
  </>,
);

export const IconCopy = svg(
  <>
    <rect x="5.4" y="5.4" width="8" height="8" rx="1.2" />
    <path d="M10.6 5.4V3.8c0-.7-.5-1.2-1.2-1.2H3.8c-.7 0-1.2.5-1.2 1.2v5.6c0 .7.5 1.2 1.2 1.2h1.6" />
  </>,
);

export const IconPlug = svg(
  <>
    <path d="M6 2.4v3.2M10 2.4v3.2" />
    <path d="M4 5.6h8v2.2a4 4 0 0 1-8 0V5.6Z" />
    <path d="M8 11.8v1.8" />
  </>,
);

export const IconUnplug = svg(
  <>
    <path d="M6 2.4v3.2M10 2.4v3.2" />
    <path d="M4 5.6h8v2.2a4 4 0 0 1-8 0V5.6Z" />
    <path d="M2.4 2.4l11.2 11.2" />
  </>,
);

export const IconKey = svg(
  <>
    <circle cx="5.6" cy="10.4" r="2.8" />
    <path d="M7.8 8.4 13 3.2M11.2 5l1.6 1.6M9.6 6.6 11 8" />
  </>,
);

export const IconLink = svg(
  <>
    <path d="M6.6 9.4a2.6 2.6 0 0 0 3.7 0l2.3-2.3a2.6 2.6 0 1 0-3.7-3.7l-1 1" />
    <path d="M9.4 6.6a2.6 2.6 0 0 0-3.7 0L3.4 8.9a2.6 2.6 0 1 0 3.7 3.7l1-1" />
  </>,
);

export const IconSearch = svg(
  <>
    <circle cx="7" cy="7" r="4.2" />
    <path d="M10.2 10.2 14 14" />
  </>,
);

export const IconWarn = svg(
  <>
    <path d="M8 2.6 14.6 13.4H1.4L8 2.6Z" />
    <path d="M8 6.6v3M8 11.4h.01" />
  </>,
);

export const IconInfo = svg(
  <>
    <circle cx="8" cy="8" r="6" />
    <path d="M8 7.2v4M8 5.1h.01" />
  </>,
);

export const IconFilter = svg(<path d="M2.4 3.4h11.2L9.4 8.6v4.4l-2.8-1.6V8.6L2.4 3.4Z" />);

export const IconColumns = svg(
  <>
    <rect x="2" y="2.8" width="12" height="10.4" rx="1.2" />
    <path d="M8 2.8v10.4" />
  </>,
);

export function IconSpinner({ size = 14, className }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      className={className}
      aria-hidden="true"
    >
      <path d="M8 1.8a6.2 6.2 0 1 0 6.2 6.2" />
    </svg>
  );
}

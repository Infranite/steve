/* Steve brand logo component ("Charcoal Enso & Gaze"), sharing single-source geometry with
 * `docs/assets/brand/*.svg`. Supports light/dark adaptive CSS tokens (`brand`) and pure
 * transparent `currentColor` monochrome (`mono`), plus a simplified `micro` silhouette tier. */
import { useId, type SVGProps } from "react"
import { cn } from "cn"
import {
  STEVE_LOGO_ENSO,
  STEVE_LOGO_LAYOUT,
  STEVE_WORDMARK_GLYPHS,
  type SteveLogoVariant,
} from "./steve-logo-paths"

export type { SteveLogoVariant } from "./steve-logo-paths"
export type SteveLogoTone = "brand" | "mono"

export interface SteveLogoProps extends Omit<SVGProps<SVGSVGElement>, "children"> {
  variant?: SteveLogoVariant
  tone?: SteveLogoTone
  /** Use the simplified micro tier without charcoal tooth filter or wash for <= 24px icons. */
  micro?: boolean
  /** Mark decorative when adjacent visible text already names Steve. */
  decorative?: boolean
  label?: string
}

export function SteveLogo({
  variant = "horizontal",
  tone = "brand",
  micro = false,
  decorative = false,
  label = "Steve",
  className,
  ...rest
}: SteveLogoProps) {
  const rawId = useId()
  const filterId = `steve-logo-${rawId.replace(/[^a-zA-Z0-9_-]/g, "")}`
  const layout = STEVE_LOGO_LAYOUT[variant]
  const isMono = tone === "mono"
  const useMicro = micro || isMono
  const showFilter = !isMono && !micro && layout.markTransform !== null

  const ink = isMono ? "currentColor" : "var(--steve-ink)"
  const accent = isMono ? "currentColor" : "var(--steve-accent)"

  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox={layout.viewBox}
      role={decorative ? undefined : "img"}
      aria-hidden={decorative ? true : undefined}
      aria-label={decorative ? undefined : label}
      data-steve-logo={variant}
      data-steve-logo-tone={tone}
      className={cn("shrink-0", className)}
      {...rest}
    >
      {showFilter && (
        <defs>
          <filter id={filterId} filterUnits="userSpaceOnUse" x="-56" y="-56" width="112" height="112">
            <feTurbulence type="fractalNoise" baseFrequency="0.115" numOctaves={3} seed={7} result="n" />
            <feDisplacementMap in="SourceGraphic" in2="n" scale={1.55} xChannelSelector="R" yChannelSelector="G" />
          </filter>
        </defs>
      )}
      {layout.markTransform && (
        <g transform={layout.markTransform}>
          {useMicro ? (
            <g>
              <path d={STEVE_LOGO_ENSO.micro.ensoPath} fill={ink} />
              <path
                d={STEVE_LOGO_ENSO.micro.sparkArc}
                fill="none"
                stroke={accent}
                strokeWidth={micro ? 4.8 : 4.2}
                strokeLinecap="round"
                opacity={isMono ? 0.9 : 1}
              />
              <path d={STEVE_LOGO_ENSO.micro.eyeL} fill={ink} />
              <path d={STEVE_LOGO_ENSO.micro.eyeR} fill={ink} />
            </g>
          ) : (
            <g filter={`url(#${filterId})`}>
              <path
                d={STEVE_LOGO_ENSO.full.ensoPath}
                fill="var(--steve-wash)"
                opacity="var(--steve-wash-opacity)"
                transform="translate(2.6, 2.8)"
              />
              <path
                d={STEVE_LOGO_ENSO.full.echoArc}
                fill="none"
                stroke={ink}
                strokeWidth={1.8}
                strokeLinecap="round"
                opacity={0.46}
              />
              <path d={STEVE_LOGO_ENSO.full.ensoPath} fill={ink} opacity={0.96} />
              <path
                d={STEVE_LOGO_ENSO.full.sparkArc}
                fill="none"
                stroke={accent}
                strokeWidth={4.5}
                strokeLinecap="round"
                opacity={0.95}
              />
              <path d={STEVE_LOGO_ENSO.full.eyeL} fill={ink} opacity={0.96} />
              <path d={STEVE_LOGO_ENSO.full.eyeR} fill={ink} opacity={0.96} />
            </g>
          )}
        </g>
      )}
      {layout.wordmarkTransform && (
        <g
          transform={layout.wordmarkTransform}
          fill={ink}
          stroke={ink}
          strokeWidth={STEVE_WORDMARK_GLYPHS.softenStrokeWidth}
          strokeLinejoin="round"
          strokeLinecap="round"
        >
          {STEVE_WORDMARK_GLYPHS.items.map((item, idx) => (
            <path key={idx} transform={`translate(${item.x}, 0)`} d={item.d} />
          ))}
        </g>
      )}
    </svg>
  )
}

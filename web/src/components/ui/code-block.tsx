import { CodeBlock as AffectCodeBlock, type CodeBlockProps } from "@/components/affect/code-block-clean"
import { useT } from "@/i18n"

/* Steve's code block, drawn by affectUI's Code Block (components/affect/code-block-clean.tsx): code on Steve's code
 * surface with a hairline, a header naming the file or language with a copy button, line numbers, marked lines, a
 * diff's added and removed lines (marks, added for Steve) and a small colorizer for TS/JS, JSON, Bash and Python.
 * It takes the code as a string, so it replaces the composable shadcn block (CodeBlockContent, CodeBlockLine,
 * CodeBlockCopyButton…); its two callers, the work trail's tool detail and the upgrade command, pass props now.
 * The copy button speaks the reader's language. */
function CodeBlock({ labels, ...props }: CodeBlockProps) {
  const t = useT()
  const name = props.filename || props.language
  const code = name ? t("common:a11y.code", { name }) : t("common:a11y.codeUnnamed")
  return <AffectCodeBlock data-slot="code-block" labels={{ copy: t("common:action.copy"), copied: t("common:action.copied"), code, ...labels }} {...props} />
}

export { CodeBlock, type CodeBlockProps }

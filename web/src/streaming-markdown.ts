/** Keep the lexer on the changing tail, even when repair of incomplete markup changes
 * the previous suffix or another message uses Streamdown's shared lexer cache. */
export function streamingBlockParser(parse: (source: string) => string[]) {
  let stable: string[] = [], prefix = ""
  return (source: string): string[] => {
    // Definitions and footnotes can change earlier blocks. CR inputs use the library's normalization.
    if (/\]:|\[\^|\r/.test(source)) {
      stable = []; prefix = ""
      return parse(source)
    }
    if (!source.startsWith(prefix)) { stable = []; prefix = "" }
    const tail = parse(source.slice(prefix.length))
    const blocks = [...stable, ...tail]
    // As in Streamdown, keep at least two blocks of look-behind and cut only at a
    // lexer-produced blank-line boundary (never at a blank line inside a fence/list).
    const cut = tail.findLastIndex((block, index) => index < tail.length - 2 && block.endsWith("\n\n")) + 1
    if (cut) {
      const closed = tail.slice(0, cut)
      stable = [...stable, ...closed]
      prefix += closed.join("")
    }
    return blocks
  }
}

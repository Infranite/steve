/* waits until every finite animation on the page has finished: details, collapsibles, dialogs and menus animate now,
 * so text read straight after opening one can still be mid-transition. The running dot loops forever and is ignored. */
export const settle = (page) => page.waitForFunction(() => document.getAnimations().every((a) => a.playState !== "running" || a.effect?.getTiming().iterations === Infinity))

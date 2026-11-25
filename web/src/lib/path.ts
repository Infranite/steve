/** Replaces a leading macOS (`/Users/<name>`) or Linux (`/home/<name>`) home directory with `~`. */
export const homeTilde = (p: string) => p.replace(/^\/(?:Users|home)\/[^/]+/, "~")

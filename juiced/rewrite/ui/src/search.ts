/**
 * Every word of the query starts a word in the text, ignoring case: "male" finds "Male" but not
 * "Female", and "q4" finds "Qwen-Q4_K_M". Every console search over a list matches this way.
 */
export function wordsMatch(text: string, query: string): boolean {
  const haystack = text.toLowerCase();
  return query.toLowerCase().split(/\s+/).filter(Boolean).every((word) => {
    for (let at = haystack.indexOf(word); at >= 0; at = haystack.indexOf(word, at + 1)) if (at === 0 || !/[\p{L}\p{N}]/u.test(haystack[at - 1]!)) return true;
    return false;
  });
}

// Which of the reader's finds is the plate. The plate detector is trained on
// plate-shaped rectangles of text, and a dealer placard in the plate holder
// ("RUBLE TRUCK SALES") is exactly that, so the reader hands back words as
// well as plates. The rules here keep plates and drop signage:
//
//  - a plate on a commercial vehicle has at least one digit; a word has none
//  - a plate is 4 to 8 characters once spaces and dashes are gone
//  - words on the yard's own ignore list are signage
//  - the same text found in two places in one picture is signage: a truck
//    carries one rear plate, but a placard is on both mud flaps
const MIN_LENGTH = 4;
const MAX_LENGTH = 8;

const normalize = (text) => String(text || '').toUpperCase().replace(/[^A-Z0-9]/g, '');

const ignoreList = (words) => (Array.isArray(words) ? words : String(words || '').split(/[,\s]+/))
  .map(normalize).filter(Boolean);

// Does this read look like a plate at all? opts: { requireDigit, ignoreWords }.
const looksLikePlate = (text, { requireDigit = true, ignoreWords = '' } = {}) => {
  const t = normalize(text);
  if (t.length < MIN_LENGTH || t.length > MAX_LENGTH) return false;
  if (requireDigit && !/\d/.test(t)) return false;
  if (ignoreList(ignoreWords).includes(t)) return false;
  return true;
};

// The best plate among the reader's finds, or null. Each find is
// { text, confidence, box }; the box tells two placards apart from one plate.
const pickPlate = (plates, opts = {}) => {
  const candidates = (plates || []).filter((p) => looksLikePlate(p.text, opts));
  const seen = new Map();
  for (const p of candidates) {
    const t = normalize(p.text);
    seen.set(t, (seen.get(t) || 0) + 1);
  }
  const single = candidates.filter((p) => seen.get(normalize(p.text)) === 1);
  if (!single.length) return null;
  return single.slice().sort((a, b) => b.confidence - a.confidence)[0];
};

module.exports = { looksLikePlate, pickPlate, normalize };

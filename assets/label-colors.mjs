// OKLab matrices: https://bottosson.github.io/posts/oklab/#converting-from-linear-srgb-to-oklab
// This fixed L/C stays inside sRGB from red (25°) to violet (305°).
const LIGHTNESS = 0.60, CHROMA = 0.10;

export function labelHue(hex) {
  if (!/^#[0-9a-f]{6}$/i.test(hex || '')) return null;
  const [r,g,b] = hex.slice(1).match(/../g).map(channel => {
    const value = parseInt(channel,16)/255;
    return value <= 0.04045 ? value/12.92 : ((value+0.055)/1.055)**2.4;
  });
  const l = Math.cbrt(0.4122214708*r+0.5363325363*g+0.0514459929*b);
  const m = Math.cbrt(0.2119034982*r+0.6806995451*g+0.1073969566*b);
  const s = Math.cbrt(0.0883024619*r+0.2817188376*g+0.6299787005*b);
  const a = 1.9779984951*l-2.4285922050*m+0.4505937099*s;
  const bb = 0.0259040371*l+0.7827717662*m-0.8086757660*s;
  if (Math.hypot(a,bb) < 0.0001) return null;
  const hue = (Math.atan2(bb,a)*180/Math.PI+360)%360;
  return hue >= 345 ? 25 : Math.max(25,Math.min(305,Math.round(hue/5)*5));
}

export function labelHex(hue) {
  const angle = hue*Math.PI/180, a = CHROMA*Math.cos(angle), b = CHROMA*Math.sin(angle);
  const l = (LIGHTNESS+0.3963377774*a+0.2158037573*b)**3;
  const m = (LIGHTNESS-0.1055613458*a-0.0638541728*b)**3;
  const s = (LIGHTNESS-0.0894841775*a-1.2914855480*b)**3;
  return '#'+[
    4.0767416621*l-3.3077115913*m+0.2309699292*s,
    -1.2684380046*l+2.6097574011*m-0.3413193965*s,
    -0.0041960863*l-0.7034186147*m+1.7076147010*s,
  ].map(value => Math.round(255*(value <= 0.0031308 ? 12.92*value : 1.055*value**(1/2.4)-0.055)).toString(16).padStart(2,'0')).join('');
}

export function labelCss(hue) {
  return `oklch(${LIGHTNESS} ${CHROMA} ${hue})`;
}

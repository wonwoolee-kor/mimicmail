import assert from 'node:assert/strict';
import {test} from 'node:test';
import {labelHue,labelHex,labelCss} from '../assets/label-colors.mjs';

function linearRgb(hex) {
  return hex.slice(1).match(/../g).map(channel => {
    const v = parseInt(channel,16)/255;
    return v <= 0.04045 ? v/12.92 : ((v+0.055)/1.055)**2.4;
  });
}

test('legacy RGB colors retain their OKLab hue within the red-to-violet range', () => {
  assert.equal(labelHue('#ff0000'),30);
  assert.equal(labelHue('#00ff00'),140);
  assert.equal(labelHue('#0000FF'),265);
  assert.equal(labelHue('#ff00ff'),305);
  assert.equal(labelHue('#ff0066'),25);
  for (const value of ['',null,'red','#123','#000000','#ffffff','#808080']) {
    assert.equal(labelHue(value),null);
  }
});

test('every selectable hue survives saving and reopening without drift', () => {
  for (let hue=25;hue<=305;hue+=5) {
    const hex = labelHex(hue);
    assert.match(hex,/^#[0-9a-f]{6}$/);
    assert.equal(labelHue(hex),hue,`${hue}: ${hex}`);
    assert.equal(labelHex(labelHue(hex)),hex);
    assert.match(labelCss(hue),new RegExp(`^oklch\\(0\\.6 [0-9.]+ ${hue}\\)$`));
  }
});

test('the complete hue range preserves lightness, chroma, gamut, and large bold white text contrast', () => {
  for (let hue=25;hue<=305;hue++) {
    const hex = labelHex(hue);
    assert.match(hex,/^#[0-9a-f]{6}$/);
    const [r,g,b] = linearRgb(hex);
    const l = Math.cbrt(0.4122214708*r+0.5363325363*g+0.0514459929*b);
    const m = Math.cbrt(0.2119034982*r+0.6806995451*g+0.1073969566*b);
    const s = Math.cbrt(0.0883024619*r+0.2817188376*g+0.6299787005*b);
    const L = 0.2104542553*l+0.7936177850*m-0.0040720468*s;
    const C = Math.hypot(1.9779984951*l-2.4285922050*m+0.4505937099*s,0.0259040371*l+0.7827717662*m-0.8086757660*s);
    assert.ok(Math.abs(L-0.60)<0.002,`lightness at ${hue}`);
    assert.ok(C>=0.10 && C<=0.122,`chroma at ${hue}`);
    assert.ok(1.05/(0.2126*r+0.7152*g+0.0722*b+0.05)>=3,`contrast at ${hue}`);
  }
});

test('red and violet gain chroma without changing lightness', () => {
  for (const hue of [25,305]) assert.equal(labelCss(hue),`oklch(0.6 0.12 ${hue})`);
});

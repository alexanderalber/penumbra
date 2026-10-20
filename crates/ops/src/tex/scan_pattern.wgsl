// tex.scan_pattern: picture `pattern` of a structured light scan at this node's size: 0 all
// white, 1 all black, then per bit of the column index (most significant first) its Gray-code
// stripes and their inverse, then the same for the row index; black past the last.

fn pn_bits(side: u32) -> u32 {
    if (side <= 1u) {
        return 1u;
    }
    return firstLeadingBit(side - 1u) + 1u;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let size = vec2u(pn.resolution);
    let p = vec2u(floor(v.pos.xy));
    let bx = pn_bits(size.x);
    let by = pn_bits(size.y);
    let i = params.pattern;
    var on = false;
    if (i == 0) {
        on = true;
    } else if (i >= 2) {
        let k = u32(i - 2);
        let bit = k / 2u;
        if (bit < bx) {
            on = (((p.x ^ (p.x >> 1u)) >> (bx - 1u - bit)) & 1u) == 1u;
        } else if (bit < bx + by) {
            on = (((p.y ^ (p.y >> 1u)) >> (by - 1u - (bit - bx))) & 1u) == 1u;
        } else {
            return vec4f(0.0, 0.0, 0.0, 1.0);
        }
        if ((k & 1u) == 1u) {
            on = !on;
        }
    }
    let c = select(0.0, params.level, on);
    return vec4f(c, c, c, 1.0);
}

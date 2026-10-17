// Builds Bird's app icon from icon-art.webp (a common swift on a plain sky)
// and writes an .iconset folder (turn it into Bird.icns with
// `iconutil -c icns`). The sky is keyed out and the bird placed on a warm
// brown background. Run by build.sh:
//   swift app/make-icon.swift <icon-art.webp> <out.iconset> [preview.png]
import AppKit

let srgb = CGColorSpace(name: CGColorSpace.sRGB)!

func rgb(_ hex: Int) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
}

/// The bird from `art` on a transparent background. The sky is the colour
/// along the edges; how much of it a pixel holds follows from how blue it is
/// compared to the sky (the bird is brown and grey), and that part is
/// subtracted again so feather edges don't keep a blue fringe.
func keyOutSky(_ art: CGImage) -> CGImage {
    let w = art.width, h = art.height
    let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8,
                        bytesPerRow: w * 4, space: srgb,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.draw(art, in: CGRect(x: 0, y: 0, width: w, height: h))
    let px = ctx.data!.assumingMemoryBound(to: UInt8.self)

    var sky = [0.0, 0.0, 0.0]
    var n = 0.0
    for i in 0..<w {
        for (x, y) in [(i, 0), (i, h - 1), (0, i * h / w), (w - 1, i * h / w)] {
            for c in 0..<3 { sky[c] += Double(px[(y * w + x) * 4 + c]) }
            n += 1
        }
    }
    sky = sky.map { $0 / n }
    let skyBlueness = sky[2] - sky[0]

    for i in 0..<(w * h) {
        let p = i * 4
        let c = (0..<3).map { Double(px[p + $0]) }
        let key = (c[2] - c[0]) / skyBlueness  // 1 = pure sky, ≤ 0 = bird
        let alpha = min(1, max(0, (1 - key) / 0.9))
        if alpha < 0.02 {
            for k in 0..<4 { px[p + k] = 0 }
            continue
        }
        for k in 0..<3 {
            // Unmix: c = alpha·bird + (1 − alpha)·sky, stored premultiplied.
            let bird = min(255, max(0, (c[k] - (1 - alpha) * sky[k]) / alpha))
            px[p + k] = UInt8(bird * alpha)
        }
        px[p + 3] = UInt8(alpha * 255)
    }
    return ctx.makeImage()!
}

/// The icon on a 1024×1024 canvas (y up), following the macOS icon grid:
/// an 824×824 rounded square with a soft shadow.
func drawIcon(in ctx: CGContext, bird: CGImage) {
    let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
    let tilePath = CGPath(roundedRect: tile, cornerWidth: 185, cornerHeight: 185, transform: nil)

    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 28,
                  color: CGColor(gray: 0, alpha: 0.30))
    ctx.addPath(tilePath)
    ctx.setFillColor(rgb(0xA57F5D))
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(tilePath)
    ctx.clip()
    // Warm brown, lighter in the middle.
    let background = CGGradient(colorsSpace: srgb,
                                colors: [rgb(0xD6B08A), rgb(0xC39B75), rgb(0x9E7856)] as CFArray,
                                locations: [0, 0.55, 1])!
    ctx.drawRadialGradient(background, startCenter: CGPoint(x: 512, y: 540), startRadius: 0,
                           endCenter: CGPoint(x: 512, y: 540), endRadius: 600, options: [])
    ctx.interpolationQuality = .high
    ctx.draw(bird, in: CGRect(x: 152, y: 152, width: 720, height: 720))
    ctx.restoreGState()
}

func render(size: Int, bird: CGImage) -> Data {
    let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
                        bytesPerRow: 0, space: srgb,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.scaleBy(x: CGFloat(size) / 1024, y: CGFloat(size) / 1024)
    drawIcon(in: ctx, bird: bird)
    let rep = NSBitmapImageRep(cgImage: ctx.makeImage()!)
    return rep.representation(using: .png, properties: [:])!
}

let args = CommandLine.arguments
guard args.count >= 3,
      let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: args[1]) as CFURL, nil),
      let art = CGImageSourceCreateImageAtIndex(source, 0, nil)
else {
    FileHandle.standardError.write(
        "usage: make-icon.swift <icon-art.webp> <out.iconset> [preview.png]\n".data(using: .utf8)!)
    exit(2)
}
let bird = keyOutSky(art)
let iconset = URL(fileURLWithPath: args[2])
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for points in [16, 32, 128, 256, 512] {
    try render(size: points, bird: bird)
        .write(to: iconset.appendingPathComponent("icon_\(points)x\(points).png"))
    try render(size: points * 2, bird: bird)
        .write(to: iconset.appendingPathComponent("icon_\(points)x\(points)@2x.png"))
}
if args.count >= 4 {
    try render(size: 1024, bird: bird).write(to: URL(fileURLWithPath: args[3]))
}

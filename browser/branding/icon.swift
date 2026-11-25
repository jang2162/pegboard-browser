// Draws the Pegboard app icon as a PNG: a rounded board with a grid of holes and one peg.
// Follows the macOS icon grid (an 824/1024 body with an outer margin and a soft shadow).
// Small sizes (under 64 px) use a 3 x 3 grid and larger holes so the shape still reads at 16 px.
// Usage: swift icon.swift <size> <out.png>
import AppKit

let args = CommandLine.arguments
guard args.count == 3, let size = Int(args[1]) else {
    FileHandle.standardError.write("usage: icon.swift <size> <out.png>\n".data(using: .utf8)!)
    exit(2)
}
let s = CGFloat(size)
guard let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
    bytesPerRow: 0, bitsPerPixel: 0)
else { exit(1) }

func rgb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat, _ a: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: r / 255, green: g / 255, blue: b / 255, alpha: a)
}

func circle(_ c: NSPoint, _ r: CGFloat) -> NSBezierPath {
    NSBezierPath(ovalIn: NSRect(x: c.x - r, y: c.y - r, width: 2 * r, height: 2 * r))
}

NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSGraphicsContext.current?.imageInterpolation = .high

// The board: 824/1024 of the canvas, centred, continuous-looking corners.
let small = size < 64
let inset = s * (small ? 60 : 100) / 1024
let body = NSRect(x: inset, y: inset * 1.15, width: s - 2 * inset, height: s - 2 * inset)
let w = body.width
let board = NSBezierPath(roundedRect: body, xRadius: w * 0.225, yRadius: w * 0.225)

NSGraphicsContext.saveGraphicsState()
let shadow = NSShadow()
shadow.shadowColor = NSColor.black.withAlphaComponent(0.28)
shadow.shadowOffset = NSSize(width: 0, height: -s * 0.012)
shadow.shadowBlurRadius = s * 0.025
shadow.set()
rgb(232, 120, 40).setFill()
board.fill()
NSGraphicsContext.restoreGraphicsState()
NSGradient(starting: rgb(250, 160, 72), ending: rgb(222, 98, 30))!.draw(in: board, angle: -90)

// The holes.
let n = small ? 3 : 4
let margin = w * (small ? 0.24 : 0.19)
let step = (w - 2 * margin) / CGFloat(n - 1)
let holeR = w * (small ? 0.085 : 0.05)
func centre(_ i: Int, _ j: Int) -> NSPoint {
    NSPoint(x: body.minX + margin + CGFloat(i) * step, y: body.minY + margin + CGFloat(j) * step)
}
let peg = (n - 1, n - 1)  // the top right hole holds the peg
for i in 0..<n {
    for j in 0..<n where (i, j) != peg {
        let c = centre(i, j)
        if !small {
            rgb(255, 214, 160, 0.55).setFill()  // a lit lower rim
            circle(NSPoint(x: c.x, y: c.y - holeR * 0.18), holeR).fill()
        }
        rgb(110, 42, 8).setFill()
        circle(c, holeR).fill()
    }
}

// The peg: a short shaft out of its hole toward the viewer (down and left), a round head.
let hole = centre(peg.0, peg.1)
let headR = w * (small ? 0.12 : 0.078)
let head = NSPoint(x: hole.x - step * 0.36, y: hole.y - step * 0.36)
rgb(110, 42, 8).setFill()
circle(hole, holeR).fill()

NSGraphicsContext.saveGraphicsState()
let pegShadow = NSShadow()
pegShadow.shadowColor = NSColor.black.withAlphaComponent(0.35)
pegShadow.shadowOffset = NSSize(width: w * 0.03, height: -w * 0.03)
pegShadow.shadowBlurRadius = w * 0.03
pegShadow.set()
let shaft = NSBezierPath()
shaft.move(to: hole)
shaft.line(to: head)
shaft.lineWidth = holeR * (small ? 1.3 : 1.6)
shaft.lineCapStyle = .round
rgb(236, 236, 240).setStroke()
shaft.stroke()
rgb(255, 255, 255).setFill()
circle(head, headR).fill()
NSGraphicsContext.restoreGraphicsState()

let headPath = circle(head, headR)
NSGradient(starting: rgb(255, 255, 255), ending: rgb(214, 216, 226))!.draw(in: headPath, angle: -60)
if !small {
    rgb(196, 198, 210).setStroke()
    headPath.lineWidth = w * 0.006
    headPath.stroke()
}

NSGraphicsContext.restoreGraphicsState()
guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
try png.write(to: URL(fileURLWithPath: args[2]))

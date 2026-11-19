// The number of a process's largest window on screen, for `screencapture -l`.
import CoreGraphics
let pid = Int(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
var best: (Int, Double)? = nil
for w in list where (w[kCGWindowOwnerPID as String] as? Int) == pid && (w[kCGWindowLayer as String] as? Int) == 0 {
    guard let b = w[kCGWindowBounds as String] as? [String: Any] else { continue }
    let area = (b["Width"] as? Double ?? 0) * (b["Height"] as? Double ?? 0)
    if area > (best?.1 ?? 0), let n = w[kCGWindowNumber as String] as? Int { best = (n, area) }
}
if let best { print(best.0) }

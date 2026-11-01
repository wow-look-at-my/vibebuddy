import Foundation

/// The compile-time switch for every update path of the Mac app: Sparkle, the daemon's update checks and every
/// control or banner that belongs to them. With it false none of them starts or shows.
public enum UpdateSwitch {
    public static let compiledIn = false
}

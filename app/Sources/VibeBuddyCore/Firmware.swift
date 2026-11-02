import Foundation

/// How the box's firmware is shown and judged; a new one comes from a package the user picks (`FirmwarePackage`).
public enum Firmware {
    /// How a firmware is shown: version first, then the build ID; firmware older than ADR-0010 has only the build.
    public static func label(version: String?, build: String?) -> String? {
        switch (version, build) {
        case let (version?, build?): return "\(version) · \(build)"
        case let (version?, nil): return version
        case let (nil, build): return build
        }
    }

    /// The box is connected but runs other firmware (a factory box, or Muse on a box that runs it). The daemon judges
    /// it from what the box prints (docs/architecture.md, decision 17); a reported build always means it is ours.
    public static func foreign(_ device: DeviceState) -> Bool {
        device.connected && device.foreignFirmware == true && device.firmwareBuild == nil
    }
}

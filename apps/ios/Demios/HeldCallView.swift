import SwiftUI

/// 4 · A held call (design: PhoneCompanion): the full review, with no Approve or Deny in
/// this version. Every argument is shown in full, never truncated (ADR-0019 §4).
struct HeldCallView: View {
    let call: HeldCall

    var body: some View {
        List {
            Section {
                VStack(alignment: .leading, spacing: 6) {
                    Text("\(call.plugin) · \(call.job) · held \(Format.when(Format.date(unixSeconds: call.createdAt)))")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Text(call.summary).font(.title2.bold())
                }
                .padding(.vertical, 4)
                .accessibilityElement(children: .combine)
            }
            Section {
                ForEach(Arguments(json: call.arguments).rows, id: \.name) { row in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(row.name).font(.subheadline).foregroundStyle(.secondary)
                        Text(row.value)
                            .font(row.isText ? .body : .system(.body, design: .monospaced))
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                    .accessibilityElement(children: .combine)
                }
            } footer: {
                Text("To answer, open Demios on your computer. Approving from this phone with Face ID comes next.")
                    .padding(.top, 8)
            }
        }
        // The heading says what it wants; the bar doesn't repeat it.
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// A call's arguments as rows: each top-level field by name (sorted, so the order is the
/// same every time), text as it is, anything else as JSON. Arguments that aren't a JSON
/// object show as one row with the exact text.
struct Arguments {
    struct Row {
        let name: String
        let value: String
        let isText: Bool
    }

    let rows: [Row]

    init(json: String) {
        guard
            let data = json.data(using: .utf8),
            let object = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]) as? [String: Any],
            !object.isEmpty
        else {
            rows = [Row(name: "Arguments", value: json, isText: false)]
            return
        }
        rows = object.keys.sorted().map { key in
            let value = object[key]!
            if let text = value as? String { return Row(name: key, value: text.isEmpty ? "None" : text, isText: true) }
            if value is NSNull { return Row(name: key, value: "None", isText: true) }
            if let list = value as? [Any], list.isEmpty { return Row(name: key, value: "None", isText: true) }
            if let list = value as? [String] { return Row(name: key, value: list.joined(separator: ", "), isText: true) }
            return Row(name: key, value: Self.json(value), isText: false)
        }
    }

    private static func json(_ value: Any) -> String {
        guard
            JSONSerialization.isValidJSONObject(value) || value is NSNumber,
            let data = try? JSONSerialization.data(
                withJSONObject: value,
                options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes, .fragmentsAllowed]
            ),
            let text = String(data: data, encoding: .utf8)
        else { return String(describing: value) }
        return text
    }
}

package dev.rime.bukkit;

import java.util.*;

/** Small YAML subset reader/writer (block maps, block/flow lists, scalars, comments) for plugin.yml and config.yml. */
public final class Yaml {
    private Yaml() {}

    private static final class Line { final int indent; final String text; Line(int i, String t) { indent = i; text = t; } }

    public static Map<String, Object> parse(String src) {
        List<Line> lines = new ArrayList<>();
        for (String raw : src.replace("\r", "").replace("\t", "  ").split("\n")) {
            String t = stripComment(raw);
            if (t.trim().isEmpty() || t.trim().equals("---")) continue;
            int i = 0; while (i < t.length() && t.charAt(i) == ' ') i++;
            lines.add(new Line(i, t.trim()));
        }
        int[] pos = {0};
        Object o = block(lines, pos, lines.isEmpty() ? 0 : lines.get(0).indent);
        return o instanceof Map ? cast(o) : new LinkedHashMap<>();
    }

    @SuppressWarnings("unchecked") private static Map<String, Object> cast(Object o) { return (Map<String, Object>) o; }

    private static String stripComment(String s) {
        boolean sq = false, dq = false;
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '\'' && !dq) sq = !sq; else if (c == '"' && !sq) dq = !dq;
            else if (c == '#' && !sq && !dq && (i == 0 || s.charAt(i - 1) == ' ')) return s.substring(0, i);
        }
        return s;
    }

    private static Object block(List<Line> lines, int[] pos, int indent) {
        if (pos[0] < lines.size() && lines.get(pos[0]).text.startsWith("- ") || pos[0] < lines.size() && lines.get(pos[0]).text.equals("-")) {
            List<Object> list = new ArrayList<>();
            while (pos[0] < lines.size() && lines.get(pos[0]).indent == indent && lines.get(pos[0]).text.startsWith("-")) {
                String item = lines.get(pos[0]).text.substring(1).trim();
                pos[0]++;
                if (item.isEmpty() && pos[0] < lines.size() && lines.get(pos[0]).indent > indent) list.add(block(lines, pos, lines.get(pos[0]).indent));
                else list.add(scalar(item));
            }
            return list;
        }
        Map<String, Object> map = new LinkedHashMap<>();
        while (pos[0] < lines.size()) {
            Line l = lines.get(pos[0]);
            if (l.indent < indent) break;
            if (l.indent > indent) { pos[0]++; continue; }
            int colon = keyColon(l.text);
            if (colon < 0) { pos[0]++; continue; }
            String key = unquote(l.text.substring(0, colon).trim());
            String rest = l.text.substring(colon + 1).trim();
            pos[0]++;
            if (rest.isEmpty()) {
                if (pos[0] < lines.size() && (lines.get(pos[0]).indent > indent || lines.get(pos[0]).indent == indent && lines.get(pos[0]).text.startsWith("-")))
                    map.put(key, block(lines, pos, lines.get(pos[0]).indent));
                else map.put(key, null);
            } else map.put(key, scalar(rest));
        }
        return map;
    }

    private static int keyColon(String t) {
        boolean sq = false, dq = false;
        for (int i = 0; i < t.length(); i++) {
            char c = t.charAt(i);
            if (c == '\'' && !dq) sq = !sq; else if (c == '"' && !sq) dq = !dq;
            else if (c == ':' && !sq && !dq && (i + 1 == t.length() || t.charAt(i + 1) == ' ')) return i;
        }
        return -1;
    }

    private static String unquote(String s) {
        if (s.length() >= 2 && (s.startsWith("\"") && s.endsWith("\"") || s.startsWith("'") && s.endsWith("'"))) return s.substring(1, s.length() - 1);
        return s;
    }

    static Object scalar(String s) {
        if (s.startsWith("[") && s.endsWith("]")) {
            List<Object> l = new ArrayList<>();
            String in = s.substring(1, s.length() - 1).trim();
            if (!in.isEmpty()) for (String p : in.split(",")) l.add(scalar(p.trim()));
            return l;
        }
        if (s.startsWith("\"") || s.startsWith("'")) return unquote(s);
        if (s.equals("true") || s.equals("false")) return Boolean.valueOf(s);
        if (s.equals("~") || s.equals("null")) return null;
        try { return Integer.valueOf(s); } catch (NumberFormatException ignored) {}
        try { if (s.matches("-?\\d+\\.\\d+")) return Double.valueOf(s); } catch (NumberFormatException ignored) {}
        return s;
    }

    public static String dump(Map<String, Object> m) { StringBuilder b = new StringBuilder(); dump(b, m, 0); return b.toString(); }

    @SuppressWarnings("unchecked")
    private static void dump(StringBuilder b, Map<String, Object> m, int indent) {
        String pad = " ".repeat(indent);
        for (Map.Entry<String, Object> e : m.entrySet()) {
            Object v = e.getValue();
            if (v instanceof Map) { b.append(pad).append(e.getKey()).append(":\n"); dump(b, (Map<String, Object>) v, indent + 2); }
            else if (v instanceof List) {
                b.append(pad).append(e.getKey()).append(":\n");
                for (Object o : (List<Object>) v) b.append(pad).append("- ").append(fmt(o)).append('\n');
            } else b.append(pad).append(e.getKey()).append(": ").append(fmt(v)).append('\n');
        }
    }

    private static String fmt(Object o) {
        if (o == null) return "~";
        if (o instanceof String) {
            String s = (String) o;
            if (s.isEmpty() || s.matches(".*[:#\\[\\]{},&*!|>'\"%@`].*") || s.matches("true|false|null|~|-?\\d+(\\.\\d+)?")) return "'" + s.replace("'", "''") + "'";
            return s;
        }
        return String.valueOf(o);
    }
}

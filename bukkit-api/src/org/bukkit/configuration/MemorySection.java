package org.bukkit.configuration;

import java.util.*;

public class MemorySection implements ConfigurationSection {
    protected final Map<String, Object> map = new LinkedHashMap<>();
    private final String path;
    private final MemorySection root;

    protected MemorySection() { path = ""; root = this; }
    protected MemorySection(MemorySection parent, String name) { root = parent.root; path = parent.path.isEmpty() ? name : parent.path + "." + name; }

    protected Configuration defaults() { return root instanceof Configuration ? ((Configuration) root).getDefaults() : null; }

    private Object raw(String p) {
        MemorySection s = this;
        String[] parts = p.split("\\.");
        for (int i = 0; i < parts.length - 1; i++) {
            Object o = s.map.get(parts[i]);
            if (!(o instanceof MemorySection)) return null;
            s = (MemorySection) o;
        }
        return s.map.get(parts[parts.length - 1]);
    }

    @Override public Object get(String p) { return get(p, defaultFor(p)); }
    private Object defaultFor(String p) {
        Configuration d = defaults();
        if (d == null || d == root) return null;
        String full = path.isEmpty() ? p : path + "." + p;
        return d.get(full, null);
    }
    @Override public Object get(String p, Object def) { if (p.isEmpty()) return this; Object o = raw(p); return o == null ? def : o; }
    @Override public boolean contains(String p) { return get(p) != null; }
    @Override public boolean isSet(String p) { return raw(p) != null; }

    @Override public void set(String p, Object value) {
        MemorySection s = this;
        String[] parts = p.split("\\.");
        for (int i = 0; i < parts.length - 1; i++) {
            Object o = s.map.get(parts[i]);
            if (!(o instanceof MemorySection)) { if (value == null) return; MemorySection n = new MemorySection(s, parts[i]); s.map.put(parts[i], n); o = n; }
            s = (MemorySection) o;
        }
        String last = parts[parts.length - 1];
        if (value == null) s.map.remove(last);
        else if (value instanceof Map) { MemorySection n = new MemorySection(s, last); n.load(asMap(value)); s.map.put(last, n); }
        else s.map.put(last, value);
    }

    @SuppressWarnings("unchecked") private static Map<String, Object> asMap(Object o) { return (Map<String, Object>) o; }

    protected void load(Map<String, Object> m) { for (Map.Entry<String, Object> e : m.entrySet()) set(e.getKey(), e.getValue()); }

    protected Map<String, Object> toMap() {
        Map<String, Object> out = new LinkedHashMap<>();
        for (Map.Entry<String, Object> e : map.entrySet()) out.put(e.getKey(), e.getValue() instanceof MemorySection ? ((MemorySection) e.getValue()).toMap() : e.getValue());
        return out;
    }

    @Override public Set<String> getKeys(boolean deep) {
        Set<String> out = new LinkedHashSet<>();
        for (Map.Entry<String, Object> e : map.entrySet()) {
            out.add(e.getKey());
            if (deep && e.getValue() instanceof MemorySection) for (String k : ((MemorySection) e.getValue()).getKeys(true)) out.add(e.getKey() + "." + k);
        }
        return out;
    }
    @Override public Map<String, Object> getValues(boolean deep) {
        Map<String, Object> out = new LinkedHashMap<>();
        for (String k : getKeys(deep)) out.put(k, get(k));
        return out;
    }
    @Override public String getString(String p) { Object o = get(p); return o == null ? null : String.valueOf(o); }
    @Override public String getString(String p, String def) { Object o = get(p, null); return o == null ? def : String.valueOf(o); }
    private static Number num(Object o) { if (o instanceof Number) return (Number) o; try { return o == null ? null : Double.valueOf(String.valueOf(o)); } catch (NumberFormatException e) { return null; } }
    @Override public int getInt(String p) { return getInt(p, 0); }
    @Override public int getInt(String p, int def) { Number n = num(get(p)); return n == null ? def : n.intValue(); }
    @Override public long getLong(String p) { return getLong(p, 0); }
    @Override public long getLong(String p, long def) { Number n = num(get(p)); return n == null ? def : n.longValue(); }
    @Override public double getDouble(String p) { return getDouble(p, 0); }
    @Override public double getDouble(String p, double def) { Number n = num(get(p)); return n == null ? def : n.doubleValue(); }
    @Override public boolean getBoolean(String p) { return getBoolean(p, false); }
    @Override public boolean getBoolean(String p, boolean def) { Object o = get(p); return o instanceof Boolean ? (Boolean) o : o == null ? def : Boolean.parseBoolean(String.valueOf(o)); }
    @Override public List<?> getList(String p) { Object o = get(p); return o instanceof List ? (List<?>) o : null; }
    @Override public List<String> getStringList(String p) { List<String> out = new ArrayList<>(); List<?> l = getList(p); if (l != null) for (Object o : l) out.add(String.valueOf(o)); return out; }
    @Override public List<Integer> getIntegerList(String p) { List<Integer> out = new ArrayList<>(); List<?> l = getList(p); if (l != null) for (Object o : l) { Number n = num(o); if (n != null) out.add(n.intValue()); } return out; }
    @Override public ConfigurationSection getConfigurationSection(String p) { Object o = get(p); return o instanceof ConfigurationSection ? (ConfigurationSection) o : null; }
    @Override public ConfigurationSection createSection(String p) { set(p, new LinkedHashMap<String, Object>()); return getConfigurationSection(p); }
    @Override public boolean isConfigurationSection(String p) { return get(p) instanceof ConfigurationSection; }
    @Override public String getName() { int i = path.lastIndexOf('.'); return i < 0 ? path : path.substring(i + 1); }
    @Override public String getCurrentPath() { return path; }
}

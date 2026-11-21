package org.bukkit.plugin;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.*;

public final class PluginDescriptionFile {
    private final Map<String, Object> map;

    public PluginDescriptionFile(InputStream stream) throws IOException {
        ByteArrayOutputStream b = new ByteArrayOutputStream();
        byte[] buf = new byte[4096];
        int n;
        while ((n = stream.read(buf)) > 0) b.write(buf, 0, n);
        map = dev.rime.bukkit.Yaml.parse(b.toString(StandardCharsets.UTF_8));
        if (getName() == null || getMain() == null) throw new IOException("plugin.yml needs 'name' and 'main'");
    }

    public String getName() { Object o = map.get("name"); return o == null ? null : String.valueOf(o).replace(' ', '_'); }
    public String getMain() { Object o = map.get("main"); return o == null ? null : String.valueOf(o); }
    public String getVersion() { Object o = map.get("version"); return o == null ? "" : String.valueOf(o); }
    public String getDescription() { Object o = map.get("description"); return o == null ? null : String.valueOf(o); }
    public String getFullName() { return getName() + " v" + getVersion(); }
    public String getAPIVersion() { Object o = map.get("api-version"); return o == null ? null : String.valueOf(o); }
    public List<String> getAuthors() {
        List<String> out = new ArrayList<>();
        if (map.get("author") != null) out.add(String.valueOf(map.get("author")));
        if (map.get("authors") instanceof List) for (Object o : (List<?>) map.get("authors")) out.add(String.valueOf(o));
        return out;
    }
    @SuppressWarnings("unchecked")
    public Map<String, Map<String, Object>> getCommands() { return getCommandMap(); }
    @SuppressWarnings("unchecked")
    public Map<String, Map<String, Object>> getCommandMap() {
        Map<String, Map<String, Object>> out = new LinkedHashMap<>();
        if (map.get("commands") instanceof Map)
            for (Map.Entry<String, Object> e : ((Map<String, Object>) map.get("commands")).entrySet())
                out.put(e.getKey(), e.getValue() instanceof Map ? (Map<String, Object>) e.getValue() : new LinkedHashMap<>());
        return out;
    }
    /** permission name -> its "default" value */
    @SuppressWarnings("unchecked")
    public Map<String, Object> getPermissionDefaults() {
        Map<String, Object> out = new LinkedHashMap<>();
        if (map.get("permissions") instanceof Map)
            for (Map.Entry<String, Object> e : ((Map<String, Object>) map.get("permissions")).entrySet())
                if (e.getValue() instanceof Map) out.put(e.getKey(), ((Map<String, Object>) e.getValue()).get("default"));
        return out;
    }
}

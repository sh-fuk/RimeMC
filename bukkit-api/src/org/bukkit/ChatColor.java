package org.bukkit;

public enum ChatColor {
    BLACK('0'), DARK_BLUE('1'), DARK_GREEN('2'), DARK_AQUA('3'), DARK_RED('4'), DARK_PURPLE('5'), GOLD('6'), GRAY('7'),
    DARK_GRAY('8'), BLUE('9'), GREEN('a'), AQUA('b'), RED('c'), LIGHT_PURPLE('d'), YELLOW('e'), WHITE('f'),
    MAGIC('k', true), BOLD('l', true), STRIKETHROUGH('m', true), UNDERLINE('n', true), ITALIC('o', true), RESET('r');

    public static final char COLOR_CHAR = '§';
    private final char code;
    private final boolean format;
    private final String str;
    ChatColor(char c) { this(c, false); }
    ChatColor(char c, boolean f) { code = c; format = f; str = new String(new char[] { COLOR_CHAR, c }); }
    public char getChar() { return code; }
    public boolean isFormat() { return format; }
    public boolean isColor() { return !format && this != RESET; }
    @Override public String toString() { return str; }
    public static ChatColor getByChar(char c) { for (ChatColor x : values()) if (x.code == Character.toLowerCase(c)) return x; return null; }
    public static String stripColor(String input) { return input == null ? null : input.replaceAll("(?i)" + COLOR_CHAR + "[0-9A-FK-ORX]", ""); }
    public static String translateAlternateColorCodes(char alt, String text) {
        char[] b = text.toCharArray();
        for (int i = 0; i < b.length - 1; i++)
            if (b[i] == alt && "0123456789AaBbCcDdEeFfKkLlMmNnOoRrXx".indexOf(b[i + 1]) > -1) { b[i] = COLOR_CHAR; b[i + 1] = Character.toLowerCase(b[i + 1]); }
        return new String(b);
    }
    public static String getLastColors(String input) {
        StringBuilder r = new StringBuilder();
        for (int i = input.length() - 2; i >= 0; i--) {
            if (input.charAt(i) == COLOR_CHAR) {
                ChatColor c = getByChar(input.charAt(i + 1));
                if (c != null) { r.insert(0, c); if (c.isColor() || c == RESET) break; }
            }
        }
        return r.toString();
    }
}

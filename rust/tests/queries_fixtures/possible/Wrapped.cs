using System.Data;
namespace D {
  public class Wrapped : IDataReader {
    public bool Read() { return true; }
  }
  public static class Use {
    public static void Run(IDataReader reader) {
      reader.Read();
    }
  }
}

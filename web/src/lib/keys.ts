/** Shape check of a provider key before sending; the server checks again. */
export function keyProblem(key: string): string | null {
  if (key.length < 8 || key.length > 512) {
    return "A chave precisa ter de 8 a 512 caracteres.";
  }
  if (/\s/.test(key)) {
    return "A chave não pode ter espaços nem quebras de linha. Confira se copiou só a chave.";
  }
  return null;
}
